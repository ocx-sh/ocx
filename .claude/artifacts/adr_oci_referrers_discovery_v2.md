# ADR: OCI Referrers Discovery v2 (Slice 2 — External Discovery + SBOM)

## Metadata

- **Status:** Approved (supersedes the single-track verify+SBOM design) — R1 fix pass applied 2026-04-19 (see Amendment Log); status unchanged.
- **Date:** 2026-04-19
- **Deciders:** OCX core maintainers
- **Issue:** [#24 — OCI referrers API for signature and SBOM discovery](https://github.com/ocx-sh/ocx/issues/24)
- **Supersedes:** `adr_oci_referrers_discovery.md` (set old ADR Status → "Superseded by adr_oci_referrers_discovery_v2.md" in-place; old file kept for historical record)
- **Superseded-by:** n/a
- **Domain Tags:** oci, cli, security, supply-chain
- **Tech Strategy Alignment:** follows Rust 2024 + Tokio + `thiserror` + BSD sysexits exit codes Golden Path in `product-tech-strategy.md`
- **Blocked by:** Slice 1 ADR ([`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md)) + Slice 1 plan fully merged

### Amendment Log

- **2026-04-19 (R1 fix pass):** Incorporates R1 review feedback (Architect F1-F8, Spec-Compliance F1-F9, Researcher R1-R6). Status remains **Approved**; only the content below changed. Key changes: bound manifest-walk to 50 referrers via `MAX_REFERRER_WALK` (Architect F1); note CI TTL false-positive and operator override (Architect F2); document SBOM trust gap — ocx sbom does not verify signatures — in three places (Architect F3); codify S2-E precedence when multiple signature formats are valid (Architect F4); pre-select `serde-spdx` as the contingency if spdx-rs stalls (Architect F5 + Researcher R3); remove `signature_format` from `SbomSummaryReport` (Architect F7 + Spec F8); reconcile Branch 3/4/5 labels of the cache algorithm (Architect F8); relocate `referrer_cache_corrupt` finding under ADR §JSON envelope additions for exit-code clarity (Spec F9); codify CycloneDX version discrimination via `specVersion` check (Researcher R2); add Rekor v2 and cosign `.att` tag probe handling (Researcher R1); pin spdx-rs commit hash after confirming last commit 2023-11-27 (Researcher R3); document CycloneDX 2.0 forward-compat escape hatch (Researcher R4); use "TBD" rather than "indefinite" for Notation framing (Researcher R5); note that GHCR discussion is dormant and not a blocker (Researcher R6).

## Relationship to Prior ADRs

This ADR is the **Slice-2 design record** in the two-slice plan established by
[`meta-plan_oci_referrers_signing.md`](../state/plans/meta-plan_oci_referrers_signing.md).

- **Parent ADR:** [`adr_oci_artifact_enrichment.md`](./adr_oci_artifact_enrichment.md) — media-type registry, subject-targeting rules, fallback-tag policy. Still authoritative for both slices.
- **Slice 1 ADR:** [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md) — owns `ocx package sign`, `ocx verify` (v0.3-bundle path), `oci/referrer/{mod,manifest,media_types,capability}.rs`, `oci/sign/`, `oci/verify/`, `ocx_cli/error_envelope.rs`, expanded `ClientError`, `TokenProvider` trait, full TUF-root / Rekor / Fulcio machinery. Slice 2 imports these without modification.
- **Superseded ADR:** [`adr_oci_referrers_discovery.md`](./adr_oci_referrers_discovery.md) — the single-track verify+SBOM design that conflated push-and-read and shipped a `level = "skip"` trust-policy half-product. That stance was rejected by the user on 2026-04-18. Decisions from it that remain valid have been carried forward below and renumbered into the S2-* namespace. Decisions that did not survive (trust-policy TOML file, `--require-referrers`, `level = "skip"`, `--distribution-spec` escape hatch) are called out in §"What changed from the superseded ADR".

What Slice 1 built and Slice 2 imports **as-is** (no modifications):

| Shared asset | Lives in | Consumed by Slice 2 |
|---|---|---|
| Referrer transport (`push_referrer_manifest`, `list_referrers`) | `ocx_lib/src/oci/client/transport.rs` (+ `native_transport.rs`, `test_transport.rs`) | Yes — read-side uses `list_referrers` |
| Referrer manifest shape + media-type table | `ocx_lib/src/oci/referrer/{manifest,media_types}.rs` | Yes — Slice 2 extends `media_types` with SBOM + legacy-cosign constants |
| Capability cache (`/v2/<name>/referrers/` probe + 24h CI / 1h interactive TTL) | `ocx_lib/src/oci/referrer/capability.rs` | Yes — unchanged; Slice 2 layers the new **referrer-index cache** above it |
| `ClientError` variants (Unauthorized/Forbidden/RateLimited/ServiceUnavailable/ReferrersUnsupported) + `ClassifyExitCode` | `ocx_lib/src/oci/client/error.rs` | Yes — reused for every network hop |
| Verify pipeline (v0.3 bundle) + TUF root + identity match | `ocx_lib/src/oci/verify/{pipeline,trust_root,identity,error}.rs` | Yes — `verify` command extended; the v0.3 code path is unchanged |
| Verify context accessor | `ocx_cli/src/app/context.rs::verify_context()` | Yes — the same accessor is reused and also provides the backing for Slice 2's new `sbom_context()` |
| JSON error envelope (`schema_version: 1`) | `ocx_cli/src/error_envelope.rs` | Yes — `ocx sbom` uses it verbatim; no schema bump |
| Exit-code taxonomy | `quality-rust-exit_codes.md` + Slice 1 ADR §Exit Code Taxonomy | Yes — Slice 2 adds no new variants |

Slice 2 **only adds**:

- `oci/verify/legacy_cosign.rs` — legacy cosign tag-based signature detection + parsing (new discovery path on `verify`)
- `oci/referrer/cache.rs` — referrer-index cache (distinct from the Slice-1 capability cache)
- `oci/sbom/{mod,cyclonedx,spdx,summary}.rs` — SBOM discovery + parsing
- `ocx_cli/src/command/sbom.rs` — new `ocx sbom` subcommand
- `ocx_cli/src/api/data/sbom.rs` — new `SbomSummary` Printable DTO

## Context

Slice 1 shipped the complete signing loop: `ocx package sign` produces Sigstore bundle v0.3 referrers, `ocx verify` validates them with Fulcio+Rekor enforcement against a required `--certificate-identity` / `--certificate-oidc-issuer` pair. That closes the "OCX signs + OCX verifies" loop but leaves two real gaps for 2026 users:

1. **Signatures made by other tools are invisible.** A team running cosign outside OCX (or migrating an existing cosign-signed registry into OCX) has no way to let `ocx verify` pick up those signatures. The legacy cosign format (`sha256-<digest>.sig` tag + `application/vnd.dev.cosign.artifact.sig.v1+json` media type) still dominates GHCR and Docker Hub, because those two registries do not implement the OCI 1.1 Referrers API and cosign v3 writes both formats simultaneously to maximize compatibility (per `research_oci_referrers_2026.md` §2).

2. **SBOMs attached to packages cannot be inspected.** OCX pipelines increasingly need an SBOM answer for audit/EU-CRA/EO-14028 reasons. Today users shell out to `oras discover` + `cosign download sbom` + `syft convert` + `jq`, each with a different JSON shape and exit-code convention. The referrer graph already contains the SBOM (any package signed by `syft attest` or pushed with `cosign attach sbom` writes one); OCX just needs a reader.

This ADR adds **read-side external-signature discovery** and **`ocx sbom`** on top of Slice 1's infrastructure. It ships zero new signing capability — Slice 1 remains the only path to produce an OCX-signed artifact. It ships zero trust-policy-file machinery — both slices stay flag-based; TOML trust policy remains v2+ with exit codes 78/79 reserved per Slice 1.

**SBOM trust gap — do not treat `ocx sbom` output as verified (Architect F3, mention #1 of 3):** `ocx sbom` surfaces the SBOM blob attached to the subject's referrer graph, but it does **not** verify the signature on the SBOM, does **not** verify the signature on the subject, and does **not** cross-check that the SBOM was signed by the same identity as the subject. A hostile publisher (or an attacker with registry write access) can push an SBOM referrer that does not reflect the bytes of the subject. Slice 2 operators must run `ocx verify` on the subject to establish publisher trust, and must separately reason about whether a publisher they trust for the subject is also trustworthy to produce SBOMs. This limitation is documented in the `ocx sbom --help` preamble, on the user-guide SBOM page, and reiterated in §Risks below. v3+ may add `ocx sbom --verify` that requires a valid signature on the SBOM referrer itself before printing output — out of scope for Slice 2.

## Decision Drivers

| Driver | Description |
|---|---|
| **D1 — Verify works on GHCR/Docker Hub.** | Research shows 2 of the top-10 registries (GHCR, Docker Hub) hosting the majority of open-source package traffic lack Referrers API. Silent-empty on verify is a show-stopper; users will uninstall. Asymmetric read-fallback is mandatory. |
| **D2 — No write-side fallback regression.** | Parent ADR and Slice 1 S1-F forbid OCX writing fallback tags. Slice 2 only *reads* them. The "no write fallback" single-source-of-truth property is preserved. |
| **D3 — SBOM is discovery, not scanning.** | `ocx sbom` surfaces what's attached; it does not maintain a CVE database, run vulnerability analysis, or generate SBOMs locally. That scope belongs to `trivy` / `grype` / `syft`. |
| **D4 — Parse enough, not all.** | Full CycloneDX 1.6 / SPDX 3.0 / in-toto predicate validation is hundreds of fields of mostly-optional metadata. OCX extracts a stable Summary struct (name, version, component count, license histogram, toolchain) — sufficient for 95 % of automation and small enough to schema-freeze. |
| **D5 — Cache referrer lists.** | Referrer-index responses can be large (fallback tag lists on GHCR routinely return 10+ entries per subject). Repeated `ocx verify` in a CI matrix hammering the same subject wastes network and hits rate limits. A TTL'd per-subject cache is essential. |
| **D6 — Same error envelope, no schema bump.** | Slice 1 froze `schema_version: 1`. Slice 2 must not rev it — every new error kind added by Slice 2 is an **addition to the enum values of `error_kind`** inside the same `schema_version`. That's the explicit v1 policy. |
| **D7 — No new OIDC flows, no new trust roots.** | SBOM discovery touches no signing crypto; legacy cosign verify uses the same TUF root Slice 1 already loaded. Slice 2 does not pull in new crypto dependencies. |
| **D8 — Three-layer error pattern unchanged.** | Every new failure mode flows through `Error → PackageError → PackageErrorKind`. No `anyhow` in `ocx_lib`. No ad-hoc String errors. |
| **D9 — SPDX 3.0 deferred per research.** | `research_cosign_sigstore_notation.md` §3: SPDX 3.0 (JSON-LD graph model) is not yet emitted by tools OCX users encounter; `spdx-rs` only supports 2.x; no Rust 3.0 parser exists. Shipping SPDX 2.3 is the right adoption bet. |

## Industry Context & Research

Slice 2 reuses the same research corpus as Slice 1 — no new research was required. Relevant sections:

| Artifact | Role in this ADR |
|---|---|
| `research_cosign_sigstore_notation.md` §"Dual-Format Detection Plan" | The two cosign signature formats that must be recognised at read time. |
| `research_cosign_sigstore_notation.md` §3 "SBOM Parsing" | `cyclonedx-bom = "=0.8.1"` (CycloneDX 1.3–1.5), `spdx-rs = "=0.5.5"` (SPDX 2.x), SPDX 3.0 deferred. |
| `research_cosign_sigstore_notation.md` §"What `ocx sbom` Should Display" | Columns for plain text output: name, version, component count, license histogram, toolchain. |
| `research_oci_referrers_2026.md` §2 "Registry Compat Matrix" | 8 of 10 registries have Referrers API; GHCR and Docker Hub do not (source-authoritative). |
| `research_oci_referrers_2026.md` §3 "Cosign Fallback Tag Bugs" | Cosign #4641 — fallback-index descriptor `artifactType` is `application/vnd.oci.empty.v1+json` instead of the real type. Manifest-walk classification is therefore mandatory on the fallback path. |
| `research_verify_cli_patterns.md` §"JSON shape precedent" | `schema_version: 1` at every root; stdout-structured / stderr-diagnostic separation; exit-on-empty is fine for discovery commands (oras model). |
| `codex_review_plan_oci_referrers.md` (all 9 findings) | Repo paths, Context injection, cache contract, ClientError expansion, JSON envelope, fixtures, no `cosign sign` in CI, no `trybuild` as unit test. All baked into Slice 1; Slice 2 inherits and re-asserts below. |

Peer-tool references (as of 2026-04):

- `oras discover` — the best-in-class OCI-native discovery command; our fallback semantics mirror its auto-probe behaviour exactly (Referrers API first, then manifest-walk). We do NOT copy its empty-array "success exit 0" semantics for `ocx sbom`'s `--no-sboms-found` case (see S2-F below).
- `cosign verify` — continues to be the reference implementation whose legacy `.sig` tag output we must accept. We do NOT copy its NDJSON output wart.
- `syft attest` / `cosign attach sbom` — the two most common tools producing SBOM referrers we need to detect. Both wrap bytes in a referrer manifest whose layer media type is `application/vnd.cyclonedx+json` or `application/spdx+json` (or in-toto wrapped). Slice 2 reads the layer, not the wrapper metadata.

## Considered Options

Per the meta-plan, Slice 2 has **five locked-in decisions** (S2-A through S2-E). Each gets a three-option comparison. Slice 2 also adds one minor decision (S2-F) on `ocx sbom` empty-result semantics.

### Decision S2-A — Read-side fallback tag strategy

**Chosen:** YES — read fallback tags on GHCR + Docker Hub via manifest-walk, with defensive per-manifest classification.

| Option | Pros | Cons |
|---|---|---|
| **S2-A.1 — Read fallback tags (chosen)** | Works on GHCR + Docker Hub (majority of OCX open-source package traffic); mirrors `oras discover` and `cosign verify` behaviour; parent-ADR "no write fallback" rule is preserved (read-only asymmetry) | N+1 round trips on fallback path (one per referrer, typically ≤10); cosign bug #4641 requires per-manifest fetch + classification (can't trust fallback-index `artifactType`) |
| S2-A.2 — Referrers-API-only, hard error on unsupported registries | Single code path; zero bug surface; nudges ecosystem toward Referrers API | Silent-empty on GHCR/Docker Hub = feature is useless for the majority of OCX users; rejection signal would be immediate |
| S2-A.3 — User-selectable via `--distribution-spec v1.1-referrers-api|v1.1-referrers-tag|auto` | Power users can pin strategy behind flaky proxies | Adds UX surface without solving anyone's problem; auto-probe works today; `oras` has the flag, we don't need it |

**Rationale:** D1 — feature must work on GHCR/Docker Hub on day one. The "no write fallback" parent-ADR rule targets the write path (concurrent-write races on the `sha256-<digest>` tag); the read path has no such hazard. Per `research_oci_referrers_2026.md` §3, fallback-index descriptors may have wrong `artifactType` due to cosign bug #4641 — we therefore **always fetch each referrer manifest individually and classify by the manifest body** (not by the fallback-index descriptor). The cosign legacy `.sig` tag format has no sunset date — cosign v3 mentions removal for v4 but no v4 timeline exists as of 2026-Q2. Treat the legacy format as an indefinite compatibility target for Slice 2 and v3+ planning.

**Manifest-walk algorithm (read path on registries without Referrers API):**

1. `GET /v2/<name>/manifests/sha256-<subject-digest>` — the fallback tag.
2. If 404 → "no referrers" (empty list). Normal for unsigned artifacts.
3. If 200 → parse as OCI ImageIndex. For **each** descriptor in `manifests[]`:
   - Fetch the referenced manifest by digest.
   - Classify by the manifest's own `artifactType` + layer `mediaType`, never by the fallback-index descriptor.
4. Return the classified descriptor set.

**Bound (Architect F1):** The manifest walk is hard-capped at `MAX_REFERRER_WALK = 50` descriptors per subject. A hostile or pathological registry could return a fallback-index with thousands of descriptors, turning each `ocx verify` into a DoS vector against the user's bandwidth and rate-limit quota. 50 is chosen because (a) empirical survey in `research_oci_referrers_2026.md` shows typical subjects have ≤10 referrers and top-decile outliers cap at ~25; (b) 50 leaves headroom for legitimate growth; (c) at 50 referrers of ~2KB each, the walk completes in <1s on a residential connection. If the fallback-index has more than 50 descriptors, OCX takes the first 50 in list order and emits a warning via `tracing::warn!` (not an error); a future flag `--referrer-walk-limit <N>` can override it when the operator knowingly opts into a larger walk (deferred to v3+).

Legacy cosign signatures using the even-older tag convention (subject tag + `.sig` suffix, e.g. `cmake:3.28.sig` alongside `cmake:3.28`) are also tried when the fallback tag returns 404 AND the subject tag is the tag, not a digest — this is step 2a in the Slice 2 plan. Per S2-E below, these are parsed via `oci/verify/legacy_cosign.rs`. A parallel `.att` tag (cosign attestations; `application/vnd.dev.cosign.artifact.attestation.v1+json`) MAY exist alongside `.sig` on legacy-cosign registries (Researcher R1); Slice 2 probes for it only when explicitly looking for SBOM attestations (the `ocx sbom` path), not during `ocx verify` (which targets signatures, not attestations). When `.att` is found in the SBOM path but its bundle format (DSSE) is unsupported by sigstore-rs 0.13, the referrer is classified but verification is skipped with a `tracing::warn!` and the envelope gets `sbom_unsupported_format`.

### Decision S2-B — Referrer-index cache TTL + bypass

**Chosen:** 1 h interactive / 24 h CI; `--no-cache` bypasses the referrer-index cache AND the Slice-1 capability cache.

| Option | Pros | Cons |
|---|---|---|
| **S2-B.1 — 1 h / 24 h with `--no-cache` bypass (chosen)** | Matches Slice 1's capability-cache TTL; CI matrices get cache-reuse across parallel jobs within a 24 h window; interactive flows stay fresh enough to see newly pushed signatures within the hour; `--no-cache` is a single flag for "I want fresh everything" | Two caches with same TTL doubles disk footprint modestly (per-subject cache entries are ~KB-scale) |
| S2-B.2 — No cache at all | Always fresh; trivial implementation | Every `ocx verify` in a CI matrix refetches the referrer list; rate-limiting on Docker Hub (10/h anonymous) becomes a blocker; contradicts D5 |
| S2-B.3 — Infinite cache + manual eviction via `ocx clean --referrers` | Cheapest operationally; mirrors `ocx` object cache semantics | Users see stale data after new signatures are pushed; "why doesn't my new signature show up" becomes the top support question |

**Rationale:** D5. TTL alignment with Slice 1's capability cache keeps the mental model simple: same bypass (`--no-cache`), same TTL detection heuristic (stdin-is-a-TTY OR `CI=true` from `ambient-id` — or the inline fallback detector; see Slice 1 ADR §S1-C).

**CI TTL false-positive note (Architect F2):** The 24 h CI TTL is chosen because CI matrices run the same verify command across dozens of parallel jobs that complete within an hour, then the job fleet stops, so the cache is effectively only used within a single CI "run" window. However, two false-positive scenarios produce **stale cache** that operators need to be aware of:

1. **Self-hosted runner persisted across days.** A self-hosted runner whose home directory is not ephemeral sees the same `~/.ocx/blobs/<registry>/.referrers/` for days or weeks. If a publisher adds new signatures to a subject between runs, the runner's CI-TTL cache holds the old empty referrer list for up to 24 h. Mitigation: self-hosted runner operators add `--no-cache` to their fleet's `ocx verify` invocation, or clear `~/.ocx/blobs/*/.referrers/` in the runner's pre-job cleanup step. Doc this in the user guide's "CI caveats" section.
2. **Non-CI environment with `CI=true` leaked.** Developers sometimes export `CI=true` locally to reproduce a CI failure. While set, OCX uses the 24 h TTL. Mitigation: the documented workaround is `ocx --no-cache verify ...` for that session, or `unset CI` before debugging. This is intentional — the TTL heuristic trusts `CI` as a user-controlled signal and deliberately does not second-guess it.

For both scenarios, `--no-cache` is the single operator override and is documented as such in `ocx sbom --help` and `ocx verify --help`. No configuration knob is added in v2 to tune the TTL — that is deferred to v3+ when enough operator feedback exists to choose the right knob shape.

**Cache layout:**

```
~/.ocx/blobs/<registry>/
  └── .capabilities.json              # Slice 1 (capability cache)
  └── .referrers/
       └── <repo>/
            └── <subject-digest>.json   # Slice 2 — referrer-index cache entry
```

**Cache entry schema (JSON):**

```json
{
  "schema_version": 1,
  "probed_at": "2026-04-19T12:00:00Z",
  "subject_digest": "sha256:aaaa...",
  "method": "referrers-api" | "fallback-tag" | "legacy-sig-tag" | "att-tag",
  "referrers": [
    { "digest": "sha256:bbbb...", "artifact_type": "...", "media_type": "...", "size": 1234, "source": "api"|"fallback-index"|"manifest-walk" }
  ]
}
```

`schema_version` is a separate cache-layout version from the error envelope; a breaking change to the cache schema triggers full cache invalidation on read (treated as a cache miss, logged at debug). The field's value in v1 is `1`.

`method` values: `"referrers-api"` — Referrers API returned a 200; `"fallback-tag"` — fallback index tag (`sha256-<digest>`) returned a hit; `"legacy-sig-tag"` — legacy cosign `.sig` tag returned a hit; `"att-tag"` — the `.att` probe in the SBOM pipeline (`ocx sbom` path) returned a hit (Researcher R1).

### Decision S2-C — SBOM formats accepted

**Chosen:** CycloneDX (any recent version via `cyclonedx-bom` 0.8.1) + SPDX 2.3 (via `spdx-rs` 0.5.5). SPDX 3.0 deferred.

| Option | Pros | Cons |
|---|---|---|
| **S2-C.1 — CycloneDX + SPDX 2.3 (chosen)** | Covers 95 %+ of SBOMs actually attached to OCI artifacts today; both crates are available and license-compatible; `cyclonedx-bom` is maintained by the CycloneDX org | SPDX 3.0 support will require a parallel parser when it lands (no Rust option today) |
| S2-C.2 — CycloneDX only | Smallest dependency footprint; freshest crate | SPDX 2.3 is dominant in US federal procurement contexts (EO 14028 artifact of choice); cutting SPDX would cut a compliance use case |
| S2-C.3 — All three + in-toto predicate unwrap + Syft JSON | Feature-complete | 3× the parse surface; Syft JSON is not a standards format; in-toto predicate unwrap is a deep rabbit hole; out of scope for D3 (discovery-not-scanning) |

**Rationale:** D4 + D9. `cyclonedx-bom` handles CycloneDX 1.3–1.5; 1.6 bytes pass through unchanged but do not parse (documented limitation — see CycloneDX version handling below). `spdx-rs` is effectively unmaintained as of April 2026 (last commit 2023-11-27, confirmed via research) but its 2.3 deserialization code works and is pinned exactly.

**spdx-rs stalled-crate contingency (Architect F5 + Researcher R3).** The `spdx-rs` crate has been stalled since 2023-11-27 (commit `c4b2e27` on `master`; cross-checked via the crate's crates.io page, GitHub commits, and `cargo search` as of 2026-04-19). We pin `spdx-rs = "=0.5.5"` and reference the `2023-11-27` commit hash in the `Cargo.lock` entry; the pin is intentional and must not be relaxed on Dependabot. If the crate never releases again, Slice 2 has three escape hatches in order of preference:

1. **`serde-spdx`** — the leading alternative SPDX 2.3 deserialization crate (last checked 2026-04; pure serde-over-serde_json shape; permissive license; maintained) is the **pre-selected contingency**. If during Slice 2 Phase 3 (acceptance tests) any SPDX 2.3 golden fixture fails to parse under `spdx-rs`, Slice 2 switches to `serde-spdx` without re-opening this ADR. The `SbomSummary` DTO shape is stable across the switch; only `sbom/spdx.rs` changes.
2. **Vendor the 2023-11-27 snapshot** under `external/spdx-rs/` (submodule or path dep), patch the tiny changes Slice 2 needs in place, and accept the maintenance cost. This is the fallback if `serde-spdx` also stalls or regresses.
3. **Write a minimal SPDX 2.3 deserialiser in Slice 2's `sbom/spdx.rs`** covering only the fields `SbomSummary` extracts (name, version, tool, component counts, license histogram). This is ~200 lines of serde derive; it is the last-resort option because SPDX 2.3 has subtle field-naming rules (underscores vs camelCase vs kebab-case across fields) that a minimal parser will get wrong on edge cases.

The escape hatch is triggered by Slice 2 Phase 3 findings, not pre-emptively. Slice 2 Phase 1 still pulls `spdx-rs` 0.5.5; the contingency is documented so the team doesn't re-open the ADR under time pressure if Phase 3 surfaces a parse bug.

SPDX 3.0 will require a rewrite; explicit decision to defer.

**CycloneDX version handling (Researcher R2 + R4).** `cyclonedx-bom` 0.8.1 tops out at CycloneDX 1.5 per its release notes; it rejects `specVersion: "1.6"` at deserialisation with a serde error. Slice 2's `sbom/cyclonedx.rs` parser **discriminates by `specVersion`** (read directly from the JSON root before handing to the crate): if the version is in {"1.3", "1.4", "1.5"}, parse; otherwise emit `sbom_unsupported_format` (exit 65) with the unsupported version string in the error context. This check happens **before** invoking `cyclonedx_bom::models::bom::Bom::parse_from_json` so that crate-side parse errors on 1.6+ are pre-empted with a clear OCX-namespaced error kind. Forward-compatibility escape hatch: CycloneDX 2.0 (when it lands) will almost certainly break the 1.x schema; the same `specVersion` check naturally rejects it with the same `sbom_unsupported_format` kind until a parser lands. Deferred to v3+.

**Notation status (Researcher R5).** Notation (JWS) verification remains out of scope for Slice 2 with status **TBD — no Rust library known today**. The superseded ADR's phrasing ("indefinite") was softened at R1 review because the ecosystem is not frozen: a Rust Notation client may appear. Slice 2 therefore does not add Notation to the verify pipeline but leaves the `application/jose+json` layer classifier as a fall-through (see §Forward-Compat Hooks for v3). Re-evaluation is triggered by any viable Rust Notation crate landing; no time-bound commitment is given.

**GHCR Referrers-API status (Researcher R6).** GHCR does not implement the OCI 1.1 Referrers API as of 2026-04. The upstream discussion thread is dormant (no activity since mid-2025); this is therefore treated as a **long-horizon "no" rather than a blocker** — Slice 2 plans around its absence via the fallback-tag manifest-walk path. If GHCR lands Referrers API support before Slice 2 release, the capability cache handles the transition naturally (a single `--no-cache` invocation refreshes the cached "Unsupported" verdict). No Slice 2 code change is required in that scenario.

**SbomSummary struct (the stable parsed output):**

```
SbomSummary {
  format: SbomFormat,                    // CycloneDx | Spdx23 | InTotoWrapper | Unknown
  spec_version: String,                  // "1.5" | "SPDX-2.3" | "N/A"
  generated_at: Option<DateTime<Utc>>,
  tool: Option<String>,                  // "cargo-cyclonedx 0.5.9" | "syft 1.12.0"
  root_component: Option<Component>,     // name + version + licenses
  component_count: usize,
  license_histogram: BTreeMap<String, usize>,  // "MIT" -> 34, "Apache-2.0" -> 12
  raw_layer_digest: Digest,              // For --download provenance
  raw_layer_size: u64,
}
```

### Decision S2-D — JSON error envelope policy

**Chosen:** Reuse Slice 1's envelope verbatim; same `schema_version: 1`; new error kinds are additions to the `error_kind` enum (serde snake_case strings). No schema bump.

| Option | Pros | Cons |
|---|---|---|
| **S2-D.1 — Reuse + extend enum (chosen)** | One schema for all commands; adding a kind is a minor-version consumer migration ("if you see a kind you don't recognise, treat as generic failure"); `schema_version` stays at 1 | Consumers who pinned exhaustive `error_kind` matches must widen them — but they should not have pinned exhaustively in the first place |
| S2-D.2 — Bump `schema_version` to 2 for Slice 2 | Signals new surface area | Forces consumers to re-pin; Slice 1 explicitly reserved schema stability across minor surface additions in its ADR §JSON error envelope |
| S2-D.3 — Separate envelope type per command | Maximal per-command control | Violates DRY; makes JQ queries non-portable; contradicts Slice 1's "one envelope shape" decision |

**Rationale:** D6. The envelope is already non-exhaustive by design (`error_kind` is a Rust enum with `#[serde(untagged)]` over a catch-all `Other(String)` tail). Slice 2 adds these `error_kind` values (all lowercase-snake_case):

| New `error_kind` | Context | Exit code |
|---|---|---|
| `sbom_no_referrers_found` | `ocx sbom <REF>` — subject has no referrer matching an SBOM media type | 79 |
| `sbom_parse_error` | CycloneDX/SPDX parser rejected the blob | 65 |
| `sbom_unsupported_format` | A detected referrer's layer media type is not in the supported set (e.g. SPDX 3.0 JSON-LD) | 65 |
| `sbom_download_io_error` | `--download <PATH>` could not write to path | 74 |
| `legacy_cosign_bundle_malformed` | `ocx verify` picked up a legacy `.sig` referrer that failed to parse | 65 |
| `legacy_cosign_identity_mismatch` | Legacy cosign sig cert SAN didn't match `--certificate-identity` | 80 |
| `legacy_cosign_issuer_mismatch` | Legacy cosign sig cert OIDC issuer didn't match `--certificate-oidc-issuer` | 80 |
| `referrer_cache_corrupt` | Cache file unreadable or schema mismatch; treated as miss — emitted only via `tracing::debug!`; **never sets the exit code** | n/a (internal) |

**Relocation note (Spec F9).** `referrer_cache_corrupt` is **not a user-facing error kind** — it is a debug-only tracing tag surfaced when `RUST_LOG=ocx=debug` (or equivalent) is set and the cache file fails to read or deserialise. On a cache-corruption event:

1. The cache entry is treated as a miss (Branch 1 in the cache algorithm falls through to Branch 2 / Branch 3 / Branch 4).
2. The corrupt file is overwritten on the next successful cache write.
3. A `tracing::debug!` event is emitted with the file path and the parse error; no stderr output is produced at default log levels.
4. The command exit code is determined by the subsequent branch of the algorithm, not by the corruption event.

It is listed in the envelope table above because the debug-level log record shares the envelope shape (it is structured JSON when `--format json` is in effect), but it carries no exit code. Consumers must not branch on it. This placement resolves the Spec F9 finding that the row as originally worded suggested a user-facing error with an exit code — that was unintended.

### Decision S2-E — External signature formats on verify

**Chosen:** Sigstore bundle v0.3 (Slice 1, unchanged) + legacy cosign tag-based (new, via manifest-walk detection).

| Option | Pros | Cons |
|---|---|---|
| **S2-E.1 — v0.3 + legacy cosign tag-based (chosen)** | Covers cosign < 2.0 and cosign on registries without Referrers API; matches `cosign verify` interoperability expectation; zero new crypto (same `sigstore-rs` APIs, different bundle shape) | Two parse paths on verify (both append to the same `VerifyResult.signatures: Vec<VerifyCandidate>` — see C-S2-5 frozen shape below; identity+issuer match, cert chain, Rekor SET run per-candidate) |
| S2-E.2 — v0.3 only | Simplest possible code path | `ocx verify` continues to return "no signatures" on legacy-signed GHCR/Docker Hub packages — the exact problem Slice 2 was opened to fix |
| S2-E.3 — v0.3 + legacy cosign + Notation (JWS) + DSSE wrappers | Feature-complete re signatures | No Rust Notation library (research §2); sigstore-rs 0.13 does not implement DSSE verification (research §1); doing this on a fork is a maintenance trap; v3+ concern |

**Rationale:** D1 + D7. The "legacy cosign" format is:

- **Discovery:** OCI tag `sha256-<subject-digest>.sig` (note the `.sig` suffix distinguishes from the Slice 2 referrer-fallback tag `sha256-<subject-digest>`), pointing at an OCI manifest whose `config.mediaType` is `application/vnd.dev.cosign.artifact.sig.v1+json` and whose first layer contains the signature payload.
- **Parsing:** layer payload is a raw cosign signature + annotations `dev.cosignproject.cosign/signature` and `dev.sigstore.cosign/bundle` (containing the Rekor SET).

The same Slice-1 verify pipeline (`oci/verify/pipeline.rs`) is extended with a bundle-shape detection step that dispatches to either:
- `oci/verify/pipeline.rs::verify_v03_bundle` (Slice 1, unchanged), or
- `oci/verify/legacy_cosign.rs::parse_legacy_bundle` (Slice 2, new).

Both paths emit the same `VerifyResult` type (**C-S2-5 frozen multi-candidate shape**): `VerifyResult { identifier, platform, subject_digest, signature_count, signatures: Vec<VerifyCandidate> }`. Each `VerifyCandidate` carries its own `signature_format` (`sigstore_bundle_v0_3 | cosign_legacy_v1`), `discovery_method` (`referrers_api | fallback_tag | legacy_sig_tag`), `certificate` sub-object (identity + issuer + validity), and `rekor` sub-object (integrated_at + log_index + log_id). Slice 1 always emits `signature_count=1`; Slice 2 may emit 2+ when both v0.3 and legacy referrers verify against the same subject. Callers inspect `signatures[]` rather than top-level identity/issuer fields. (Scope note — Architect F7 + Spec F8: `signature_format` applies to **`ocx verify` output only**. The `SbomSummaryReport` struct of `ocx sbom` has no `signature_format` field because `ocx sbom` does not verify signatures; see SBOM trust gap above.)

**Precedence when multiple valid signature formats exist (Architect F4).** A subject may carry both a Sigstore v0.3 bundle referrer **and** a legacy cosign `.sig` tag — cosign v3 writes both simultaneously on fallback-mode registries. `ocx verify` treats this as follows:

1. Classify each discovered candidate by format.
2. Run the full verify pipeline (identity + issuer + cert chain + Rekor) against **every** candidate independently.
3. If **any** candidate succeeds under the supplied `--certificate-identity` + `--certificate-oidc-issuer`, the command exits 0 (standard cosign semantics — "at least one valid sig is sufficient").
4. If multiple formats succeed, plain-text output shows the first one in this preference order: **v0.3 bundle first, legacy cosign second** (the rationale being that v0.3 is the forward-looking format; operators reading plain text should see the format they are being nudged toward). JSON output includes **all** successful candidates as a JSON array under `signatures[]`, each with its own `signature_format` and `discovery_method`. Consumers that need deterministic selection across formats must parse the JSON array and apply their own ordering — the CLI does not expose a `--prefer` flag for signatures (deferred v3+).
5. If all candidates fail, the command exits 79 (`no_matching_signatures_found`) with per-candidate failure reasons listed under `signatures[]` in the JSON envelope's `details` field.

Precedence is therefore: (a) operator-supplied constraints (`--certificate-identity` / `--certificate-oidc-issuer`) are the authoritative filter; (b) format is not a ranking axis for correctness — only for display order when multiple equally-valid signatures exist.

### Decision S2-F — `ocx sbom` empty-result exit code

**Chosen:** Exit 79 (`NotFound`) when no SBOM referrers are attached, consistent with Slice 1's `ocx verify` no-sig-found semantics.

| Option | Pros | Cons |
|---|---|---|
| **S2-F.1 — Exit 79 on empty (chosen)** | Consistent with `ocx verify` (both are "did the publisher attach the thing?" queries); CI scripts can branch on `$?` directly; matches Slice 1 §Exit Code 79 reservation | Discovery commands like `oras discover` exit 0 on empty — a subset of consumers will be surprised |
| S2-F.2 — Exit 0 on empty with `referrer_count: 0` in JSON | Matches `oras discover` | Breaks the "exit-code-is-answer" invariant Slice 1 established for `ocx verify` |
| S2-F.3 — Exit 0 default, `--require-sbom` opts into 79 | Covers both user groups | Adds flag surface; v2+ if needed |

**Rationale:** Symmetry with Slice 1's `ocx verify` (exit 79 when referrer list is empty). Reserving exit 0 for "SBOM found and parsed OK" keeps the `ocx verify && ocx sbom && ocx install` idiom idiomatic.

## Cache algorithm (normative)

This is the exact algorithm the referrer-index cache layer must implement. Every branch needs a unit test (see plan §Phase 3).

```
fn resolve_referrers(subject_digest, registry, repo, no_cache, offline) -> Result<ReferrerList>:

  # Branch 1 — referrer-index cache hit (skipped on --no-cache)
  if not no_cache:
    entry = read_referrer_cache(registry, repo, subject_digest)
    if entry is Some and entry.probed_at + ttl() > now():
      return entry.referrers                                 # HIT, no network

  # Branches 2 & 3 — capability-cache fast paths (skipped on --no-cache)
  if not no_cache:
    cap = read_capability_cache(registry)
    if cap == Supported:
      # Branch 2 — capability says "supported": hit Referrers API, cache result
      if offline:
        return Err(OfflineBlocked)                           # exit 81
      list = GET /v2/<repo>/referrers/<subject_digest>
      list = classify_each(list)                             # cosign #4641 defence
      write_referrer_cache(list, method="referrers-api")
      return list
    if cap == Unsupported:
      # Branch 3 — capability says "unsupported": manifest-walk, cache result
      if offline:
        return Err(OfflineBlocked)                           # exit 81
      list = manifest_walk_fallback_tags(subject_digest)     # S2-A step 1–4
      write_referrer_cache(list, method="fallback-tag")
      return list

  # Branch 4 — capability unknown (or --no-cache forced through): probe Referrers API
  if offline:
    return Err(OfflineBlocked)                               # exit 81 (covers
                                                              #         Branch 5)
  response = GET /v2/<repo>/referrers/<subject_digest>
  match response.status:
    200        -> cache_capability(Supported)                  # Branch 4a
                  list = classify_each(parse(response))
                  write_referrer_cache(list, method="referrers-api")
                  return list
    404 | 405  -> cache_capability(Unsupported)                # Branch 4b
                  list = manifest_walk_fallback_tags(subject_digest)
                  write_referrer_cache(list, method="fallback-tag")
                  return list
    401        -> Err(Unauthorized)                            # exit 80
    403        -> Err(Forbidden)                               # exit 77
    429        -> Err(RateLimited)                             # exit 75
    5xx        -> Err(ServiceUnavailable)                      # exit 69
```

**Branch-label reconciliation (Architect F8).** The original draft double-labelled the offline-cache-miss as both "Branch 4 guard" and "Branch 5 return", which was unreachable-by-construction. This rewrite folds the `OfflineBlocked` guard into each reachable branch (Branch 2, Branch 3, Branch 4) so offline handling is local to every network-crossing branch and no dead code paths remain. The `--no-cache` forced-path is no longer called a separate "Branch 6" because `--no-cache` simply skips the capability-cache fast paths and drops into Branch 4 — there is no distinct behaviour to label.

**Branch inventory → unit tests** in `oci/referrer/cache.rs` inline tests:

1. Branch 1: Fresh cache hit returns without network.
2. Branch 2: Capability-cache "supported" invokes API, writes referrer-index cache.
3. Branch 2 offline: Capability-cache "supported" + offline → exit 81.
4. Branch 3: Capability-cache "unsupported" invokes manifest-walk.
5. Branch 3 offline: Capability-cache "unsupported" + offline → exit 81.
6. Branch 4a: Capability unknown — 200 path caches Supported + refs.
7. Branch 4b: Capability unknown — 404 path caches Unsupported + falls back.
8. Branch 4 offline: Capability unknown + offline → exit 81 (equivalent to the deleted "Branch 5").
9. `--no-cache` bypasses the referrer-index cache **and** the capability cache for a single invocation, exercising Branch 4 regardless of prior state (contract from Slice 1 §Capability cache contract; Slice 2 extends it).

## Architecture Decisions

### New module shape (Slice 2 additions only)

```
crates/ocx_lib/src/
  oci/
    referrer/
      cache.rs                     ← NEW (referrer-index cache; see Cache algorithm)
    verify/
      legacy_cosign.rs             ← NEW (S2-E; parses application/vnd.dev.cosign.artifact.sig.v1+json + sha256-<digest>.sig tags)
      pipeline.rs                  ← MODIFY (dispatch: v0.3 vs legacy; VerifyResult reshaped to signature_count + signatures: Vec<VerifyCandidate> per C-S2-5)
    sbom/                          ← NEW module
      mod.rs                       ← re-exports; SbomFormat enum; SbomSummary struct
      cyclonedx.rs                 ← cyclonedx-bom 0.8.1 wrapper → SbomSummary
      spdx.rs                      ← spdx-rs 0.5.5 wrapper → SbomSummary
      summary.rs                   ← shared Component / LicenseHistogram types
  package_manager/tasks/
    sbom.rs                        ← NEW (SbomManager task)

crates/ocx_cli/src/
  app/
    context.rs                     ← MODIFY (add sbom_context() returning same (&Index, &Client) shape as verify_context())
  command/
    sbom.rs                        ← NEW
    command.rs                     ← MODIFY (add Sbom variant)
    verify.rs                      ← MODIFY (pass --certificate-identity / --certificate-oidc-issuer to legacy path too)
  api/
    data/
      sbom.rs                      ← NEW (SbomSummaryReport Printable)
    data.rs                        ← MODIFY (pub mod sbom;)
```

### Context injection

Slice 2's new `sbom_context()` accessor mirrors Slice 1 exactly (Codex finding #2):

```rust
impl Context {
    /// Returns (index, optional client) for SBOM discovery.
    ///
    /// Never errors on `remote_client.is_none()` — the offline-first contract is
    /// that callers can proceed with a cache-only lookup and only fail downstream
    /// (exit 81 `OfflineBlocked`) if both the referrer-index cache AND the
    /// referenced blobs are cold. This preserves `test_sbom_offline_with_cache_succeeds`.
    pub fn sbom_context(&self) -> ocx_lib::Result<(&oci::index::Index, Option<&oci::Client>)>;
}
```

Tag→digest resolution is via `default_index` (cache-coherent); referrer lookup + blob pull is via the optional `Client` (only present when online). This matches the Slice-1 accessor pattern renamed from `verify_context` to `online_context`; the novelty here is offline-first: the referrer-index cache may satisfy the read without touching `Client` at all, and when `remote_client` is `None`, the pipeline proceeds if and only if the cache is warm. See Codex finding C-S2-1.

### `ocx sbom` CLI surface

```
ocx sbom <REFERENCE>
    [--platform <OS/ARCH>]
    [--download <PATH>]        # write raw SBOM blob to PATH or '-' for stdout
    [--prefer cyclonedx|spdx]  # when multiple SBOMs are attached; default: cyclonedx
    [--no-cache]               # bypass referrer-index + capability caches
    [--format plain|json]
```

**SBOM trust gap — `ocx sbom --help` must say so (Architect F3, mention #2 of 3):** The `ocx sbom` help text begins with:
> Discover and parse SBOMs already attached to an OCX package. Does NOT verify the SBOM's signature or that the SBOM matches the subject — run `ocx verify` first, and understand that verifying the subject does not verify the SBOM. Does NOT generate SBOMs — use `syft` or `cargo cyclonedx`.

This is a block-tier documentation requirement in the Slice 2 plan §Step 2 CLI surface. A reviewer flagging this preamble as missing should reject the PR.

**Flag notes (aligned with feedback_flags_before_args memory):**

- All flags precede the positional identifier in docs and examples.
- `--download -` streams raw SBOM bytes to stdout; the JSON/plain summary is suppressed entirely (not emitted to stdout, not to stderr — happy-path stderr is empty). Matches the superseded ADR's Invariant 5; re-confirmed here.
- `--prefer` acts only when multiple SBOM referrers are attached with different formats. Default to CycloneDX because `cargo-cyclonedx` (the OCX dogfood SBOM generator per the SBOM ADR) emits CycloneDX.
- No `--require-*` flags in Slice 2. The superseded ADR had them; user feedback on the Slice 1 trust-policy rejection applies here too — flag-based strict behaviour is the exit code, not a flag. Callers who want "fail if no SBOM" check for exit 79.

### `ocx verify` extensions

Slice 1's `Verify` command struct is unchanged in signature. What changes:

1. `oci/verify/pipeline.rs` gains a dispatch step: after listing referrers, iterate and for each, detect format by `artifactType` + layer `mediaType`:
   - `application/vnd.dev.sigstore.bundle.v0.3+json` → Slice-1 path (unchanged).
   - `application/vnd.dev.cosign.artifact.sig.v1+json` → `legacy_cosign.rs::parse_legacy_bundle`.
   - Unknown → skip with a debug-level trace event; excluded from the count.
2. A pass succeeds if **any** signature format matches `--certificate-identity` + `--certificate-oidc-issuer` with a valid Rekor SET. Multiple sigs with some failing and some passing = overall success (standard cosign semantics).
3. `VerificationReport` JSON output follows the Slice 1 frozen success envelope (`data.signatures[]`): each element carries its own `signature_format` (`sigstore_bundle_v0_3 | cosign_legacy_v1`) and `discovery_method` (`referrers_api | fallback_tag | legacy_sig_tag`) plus per-candidate `certificate` + `rekor` objects. `data.signature_count` equals `data.signatures.len()` (**C-S2-5**).
4. When BOTH the referrer fallback-tag AND a legacy `.sig` tag exist (possible on GHCR where cosign v3 writes both), OCX verifies **both** and reports both in JSON. Plain text shows the first matching one; JSON array includes both for audit.

### Media types registered in `oci/referrer/media_types.rs` (Slice 2 additions)

| Constant | Value | Purpose |
|---|---|---|
| `COSIGN_LEGACY_SIG_V1` | `application/vnd.dev.cosign.artifact.sig.v1+json` | S2-E discovery (manifest `config.mediaType`) |
| `CYCLONEDX_JSON` | `application/vnd.cyclonedx+json` | S2-C SBOM layer |
| `SPDX_JSON` | `application/spdx+json` | S2-C SBOM layer (SPDX 2.3) |
| `IN_TOTO_JSON` | `application/vnd.in-toto+json` | SBOM attestation wrapper; Slice 2 unwraps and classifies by predicate type |

Note: Slice 1 reserved the const table for extension; Slice 2 only adds constants and does NOT refactor the surface.

### SBOM parsing algorithm

1. `list_referrers(subject_digest)` → filtered list of descriptors whose `artifactType` is in {`CYCLONEDX_JSON`, `SPDX_JSON`, `IN_TOTO_JSON`}.
2. **Probe the legacy cosign `.att` tag (Researcher R1; C-S2-2 frozen rule).** If the registry has no Referrers API support (capability cache = Unsupported), probe `GET /v2/<repo>/manifests/sha256-<digest>.att` where `<digest>` is the resolved subject manifest digest. This mirrors the cosign `.sig` convention and applies uniformly to tag-addressed and digest-addressed subjects — cosign tag-naming is digest-based by design; the probe is identical in both cases (tag references resolve to the digest first via the standard identifier resolver). If the response is 200, parse as an OCI manifest; enumerate its layers; for each layer whose media type is one of {`CYCLONEDX_JSON`, `SPDX_JSON`, `IN_TOTO_JSON`}, add it as an additional candidate to the list from step 1. If the `.att` layer uses an unsupported DSSE format that sigstore-rs 0.13 cannot unwrap, emit a `tracing::warn!` and classify the referrer as `sbom_unsupported_format` but continue (do not exit). `.att` probing is **only performed by the `ocx sbom` path**, never by `ocx verify` — verify is signature-only and attestations are the SBOM path's concern.
3. If empty (after step 1 and step 2 probing) → exit 79 (`sbom_no_referrers_found`).
4. For each candidate, if `--prefer <FORMAT>` matches the media type, select it; else first in referrer order.
5. Pull the selected manifest → pull its first layer blob.
6. Dispatch to parser by media type:
   - `application/vnd.cyclonedx+json` → `sbom/cyclonedx.rs::parse` (via `cyclonedx-bom`, with the `specVersion` pre-check described in the S2-C rationale)
   - `application/spdx+json` → `sbom/spdx.rs::parse` (via `spdx-rs`, or the `serde-spdx` contingency)
   - `application/vnd.in-toto+json` → unwrap DSSE envelope; recurse on the predicate payload
7. Return `SbomSummary`.
8. `--download <PATH>` writes the **raw layer bytes** (pre-parse) to `PATH`. `--download -` streams to stdout and suppresses the summary.

## Exit Code Taxonomy

Slice 2 introduces **no new variants**. It extends Slice 1's `ExitCode` enum via the same error mechanism: new `PackageErrorKind` variants map onto existing codes. Codex finding #4 (expanded `ClientError`) already covers every network failure path Slice 2 touches.

| Code | Used in Slice 2 for | Source |
|---|---|---|
| 0 | Success (verify match; sbom parsed; legacy bundle verified) | — |
| 64 | Usage (bad `--prefer` value, mutually exclusive flags) | Slice 1 |
| 65 | Malformed bundle / malformed SBOM | Slice 1 + new kinds. A corrupt referrer cache entry is **not** surfaced as exit 65 — it is a debug-only event (see §Error-Kind Additions `referrer_cache_corrupt`) and the exit code is determined by the branch of the cache algorithm that takes over after fall-through. |
| 69 | Registry 5xx; Referrers unsupported AND fallback tag also 5xx | Slice 1 |
| 74 | `--download <PATH>` I/O failure | Slice 1 |
| 75 | Registry 429 | Slice 1 |
| 77 | Registry 403 | Slice 1 |
| 79 | No SBOM referrers found; no matching signatures found (both slices) | Slice 1 |
| 80 | Registry 401; legacy cosign identity/issuer mismatch | Slice 1 |
| 81 | `OfflineBlocked` — offline + cache miss (see Cache algorithm §Branch 5) | Slice 1 (Resolution A) |
| 82 | Rekor unavailable while verifying a legacy cosign SET | Slice 1 |
| 83 | Registry lacks Referrers API **and** fallback-tag walk also fails (hard-fail on discovery) | Slice 1 |

**Exit-code 81 conflict resolution:** Slice 1 adopted Resolution A (`OfflineBlocked = 81` preserved; network 5xx → 69). Slice 2 inherits this unchanged. Every Slice 2 `--offline` + cache-miss path returns 81.

**Exit-code 83 usage note (Slice 2 only):** Slice 1 reserves 83 = `ReferrersUnsupported` for the unlikely case where a registry explicitly refuses Referrers AND the fallback-tag walk returns a protocol-level error (not a 404, which is a normal empty signal). Slice 2 activates this code in the Branch 4 path of the cache algorithm: if the Referrers API probe returns 404/405 (Branch 4b) and the subsequent `manifest_walk_fallback_tags` call fails with a non-404 error (e.g., network 5xx cascading into registry-unreachable), the wrapping task returns `ReferrersUnsupported` with exit 83 so scripts can distinguish "registry stack is broken" from "your package has no signatures" (79).

## What changed from the superseded ADR

| Decision in `adr_oci_referrers_discovery.md` | Fate in v2 |
|---|---|
| `--trust-policy <PATH>` TOML file with `level = "skip"` / `"strict"` / ... | **Removed.** Both slices stay flag-based. Exit codes 78 (parse) + 79 (file not found) remain reserved for v2+ per Slice 1 §S1-G. |
| `--require-referrers` / `--min-referrers N` / `--require-artifact-type MEDIA_TYPE` | **Removed.** Use exit code 79 (no referrers found) as the programmatic signal. |
| `--distribution-spec v1.1-referrers-api|v1.1-referrers-tag|auto` | **Removed.** Auto-probe is the only behavior; `--no-cache` is the only explicit knob. See S2-A. |
| Schema v1 with `verification.result = "skip"` | **Removed.** There is no `skip` in either slice. Verify always enforces; SBOM parses what it finds or errors. |
| GitHub Attestations API discovery | Still out of scope; documented as a v2+ concern unchanged. |
| Referrer fallback on read (Decision A) | **Kept** (locked as S2-A). |
| Referrer-index cache 24h/1h | **Kept** (locked as S2-B). |
| CycloneDX + SPDX 2.3 SBOM parsing | **Kept** (locked as S2-C). |
| `schema_version: 1` at root of every JSON shape | **Kept** (locked as S2-D — same envelope Slice 1 froze). |
| `sigstore = "=0.13"` pin | **Kept** (inherited from Slice 1). |
| `cyclonedx-bom = "=0.8.1"` + `spdx-rs = "=0.5.5"` | **Activated** — were provisional in v1 discovery ADR. Now load-bearing. |

## Dependencies

| Package | Version | Purpose | Notes |
|---|---|---|---|
| `cyclonedx-bom` | `=0.8.1` | CycloneDX JSON parsing (1.3–1.5) | NEW for Slice 2 (was deferred / removed in superseded ADR) |
| `spdx-rs` | `=0.5.5` | SPDX 2.3 JSON parsing | NEW for Slice 2; crate unmaintained upstream but pinned exactly |
| (existing) `sigstore` | `=0.13` | Reused for legacy cosign bundle verification | Slice 1; no bump |
| (existing) `sigstore-trust-root` | `=0.6.4` | Reused TUF root | Slice 1; no bump |
| (existing) `oci-client` (patched fork) | — | Reused for list_referrers; no new API calls | Slice 1 |

License review per `deny.toml` required in plan Phase 4 (both new crates are Apache-2.0; compatible).

## Forward-Compat Hooks for v3

- **CycloneDX 2.0 (Transparency Exchange Language)** — milestone due June 30, 2026 per CycloneDX/specification issue tracker. If backward compat with 1.x holds, a `cyclonedx-bom` crate update suffices. If the JSON root schema changes, add `sbom/cyclonedx2.rs` alongside `cyclonedx.rs` and introduce `SbomFormat::CycloneDx2` — `#[non_exhaustive]` on `SbomFormat` means no breaking change to `SbomSummary`. Implementation should be scheduled within 2 months of CycloneDX 2.0 GA to maintain discovery parity.
- **SPDX 3.0** — when a Rust SPDX 3.0 parser lands, it plugs into `sbom/` module as a sibling to `spdx.rs`; `SbomFormat::Spdx30` enum variant gets added. Breaking? No — enum is `#[non_exhaustive]`.
- **In-toto attestation deep parse** — today we unwrap the DSSE envelope and recurse on the predicate payload; v3 can parse SLSA provenance / VEX statements to richer summaries without changing `SbomSummary`'s top-level shape.
- **Notation (JWS)** — still blocked on Rust library availability. The manifest-walk classifier is prepared: `application/jose+json` layer = "notation discovered, verify out-of-band". This is a fall-through in `verify/pipeline.rs`, not a new error.
- **Trust-policy TOML** — Slice 1 reserved exit codes 78/79 and didn't ship a file format. When v2+ adds it, both slices' flag surfaces become overrides above the file.
- **`ocx clean --referrer-cache`** — the referrer-index cache lives under `~/.ocx/blobs/<registry>/.referrers/`; Slice 2 ships without a dedicated GC subcommand. v3+ adds `ocx clean --referrer-cache` following the existing `ocx clean` pattern. The directory layout is chosen so GC is a recursive remove with no schema knowledge required.

## Risks & Mitigations

| Risk | Severity | Mitigation |
|---|---|---|
| Cosign bug #4641 fixed upstream and OCX defensive parse wastes network | Low | Parse remains correct post-fix; cost bounded (N+1 round trips, N ≤ 10 in practice); acceptance test covers both pre-fix and post-fix fallback-index shapes |
| `spdx-rs` 0.5.5 unmaintained — 2.3 parse breaks on an edge SBOM | Medium | Fuzz-test inline in Slice-2 Phase 3 with SBOMs from `cargo-cyclonedx` + `syft` + hand-crafted edge cases; wrap parse in a `SbomParseError::Spdx2UnsupportedEdge` variant emitting exit 65 with a clear message naming the offending field |
| GHCR rate-limit on manifest-walk path (N+1 round trips) | Medium | 24h CI cache TTL absorbs repeated calls; honor Retry-After on 429; `--no-cache` is explicitly documented as anti-pattern for CI |
| Legacy cosign bundle parse drift | Low | Golden fixtures committed under `test/fixtures/verify/legacy/` generated once against cosign 2.x; regeneration runbook matches Slice 1's |
| Referrer-index cache grows unboundedly | Medium | 24h TTL + file-system aging; `ocx clean --referrer-cache` deferred to v3+; doc a `find ~/.ocx/blobs/*/.referrers/ -mtime +7 -delete` one-liner in user guide until then |
| User expects `ocx sbom` to produce an SBOM (confusion with `syft`) | High | `ocx sbom --help` leads with "Discover and parse SBOMs already attached to an OCX package. Does not generate SBOMs — use `syft` or `cargo cyclonedx` to produce."; user-guide doc same |
| User treats `ocx sbom` output as signed/verified (SBOM trust gap, Architect F3 mention #3 of 3) | High | `ocx sbom --help` preamble explicitly states the SBOM signature is not verified and the SBOM may not reflect the subject bytes; user-guide SBOM page calls this out in a prominent admonition block; `ocx verify` documentation reminds operators that verifying the subject does not verify the SBOM; v3+ tracked via GitHub issue for `ocx sbom --verify` (not in Slice 2 scope) |
| `spdx-rs` 0.5.5 stalls permanently and a golden fixture in Phase 3 surfaces a parse bug | Medium | Pre-selected contingency: switch `sbom/spdx.rs` to `serde-spdx`; `SbomSummary` DTO unchanged; decision does not require re-opening this ADR. Fallbacks: vendor the 2023-11-27 snapshot under `external/spdx-rs/`, or write a minimal 200-line deserialiser. Escape-hatch policy documented in S2-C rationale. |
| sigstore-rs 0.14 release lands during Slice 2 with a breaking API change | Medium | Slice 1 pins `sigstore = "=0.13"`; Slice 2 inherits the pin. If 0.14 ships while Slice 2 is in flight, Slice 2 does **not** bump — the upgrade is a separate project (runbook in Slice 1 plan §Notes). Any Slice 2 code calling sigstore-rs uses 0.13 APIs only. |
| CycloneDX 1.6 or 2.0 emitted in the wild before Slice 2 ships | Low | `sbom/cyclonedx.rs` rejects any `specVersion` outside {1.3, 1.4, 1.5} with `sbom_unsupported_format` (exit 65); the raw layer remains downloadable via `--download` so operators keep access to the bytes; v3+ bumps `cyclonedx-bom` when it supports 1.6+. CycloneDX 2.0 specifically: see §Forward-Compat Hooks for the scheduled upgrade plan — implementation within 2 months of 2.0 GA (milestone due 2026-06-30) to maintain discovery parity. |
| SBOM layer content is compressed (gzip) or encoded unexpectedly | Low | Detect by magic bytes (gzip = 0x1f 0x8b); support uncompressed + gzip; beyond that, pass-through to user via `--download` |
| Two caches (capability + referrer-index) diverge (registry adds Referrers API within 24h window) | Low | Acceptable staleness; `--no-cache` bypasses both; documented in user guide as "if a registry just enabled Referrers API, run with `--no-cache` once" |
| SBOM DTO schema v1 freeze vs. v3+ SPDX 3.0 / in-toto richness | Low | `SbomSummary` is the stable DTO; raw layer always downloadable via `--download` for callers who need richer data |
| GHCR + Docker Hub fallback-code longevity | Low | Low | GHCR discussion #163029 dormant since 2025-10; Docker Hub silent since 2022-10 blog post. Fallback code not expected obsolete in Slice 2 or v3 timeframes. No action — design correctly assumes persistent need. |

## Testing Strategy

### Unit tests

| Component | Branches |
|---|---|
| `oci/referrer/cache.rs` | 6 branches per Cache algorithm + `--no-cache` bypass (7 tests) |
| `oci/verify/legacy_cosign.rs` | v0.3 detect, legacy detect, mixed-referrers select-any-match, malformed bundle, identity mismatch (legacy), issuer mismatch (legacy), legacy sig-tag path (`cmake:3.28.sig`) |
| `oci/sbom/cyclonedx.rs` | CycloneDX 1.3 / 1.4 / 1.5 golden fixtures round-trip; 1.6 input → `sbom_unsupported_format`; malformed JSON → `sbom_parse_error`; empty components array → zero count |
| `oci/sbom/spdx.rs` | SPDX 2.3 minimal doc; SPDX 2.2 input → accepted (backward-compat); SPDX 3.0 JSON-LD → `sbom_unsupported_format` |
| `oci/sbom/summary.rs` | license histogram dedup; root-component extraction; generator-tool parse |

### Acceptance tests

| File | Fixture | Assertion |
|---|---|---|
| `test/tests/test_sbom.py` | Manually-populated `registry:2` with a pre-generated CycloneDX blob attached as a referrer to a published OCX package | `ocx sbom <ref>` exits 0; JSON matches schema; `component_count >= 1`; `format == "cyclonedx"` |
| `test/tests/test_sbom.py::test_download_stdout` | Same fixture | `ocx sbom <ref> --download -` bytes match the layer digest; no stdout text report |
| `test/tests/test_sbom.py::test_no_sbom` | Unsigned package (no SBOM referrer) | exits 79; JSON envelope `error_kind == "sbom_no_referrers_found"` |
| `test/tests/test_verify_legacy.py` | `test/fixtures/verify/legacy/bundle.tar` — pre-generated cosign ≥2.0 signature in legacy format (committed bytes; regenerable via runbook) | `ocx verify --certificate-identity X --certificate-oidc-issuer Y <ref>` exits 0; JSON reports `data.signature_count=1`; `data.signatures[0].signature_format="cosign_legacy_v1"` (**C-S2-5 per-candidate shape**) |
| `test/tests/test_verify_legacy.py::test_fallback_manifest_walk` | Mock `registry:2` stubbed to return 404 on `/v2/<name>/referrers/*` and 200 on `/v2/<name>/manifests/sha256-<digest>` | Verify picks up the legacy bundle via manifest-walk; JSON reports `data.signatures[0].discovery_method="fallback_tag"` |
| `test/tests/test_verify_legacy.py::test_mixed_formats` | Same subject with BOTH a v0.3 referrer AND a legacy `.sig` tag | Verify succeeds; JSON includes both signatures; both counted |

Both tests run in the existing `test/` pytest harness using `registry` + `package` fixtures. Per Codex finding #8, **no live cosign invocation** in CI; fixtures are committed bytes with a regeneration runbook at `test/fixtures/verify/legacy/README.md` (structurally identical to Slice 1's runbook).

### Integration (opt-in)

- `OCX_TEST_LIVE_GHCR=1` — run `ocx verify` + `ocx sbom` against a known-signed public package on `ghcr.io`. Skipped by default.

## Not Doing (Slice 2 scope guardrails)

- **Writing SBOMs (`ocx package attest`)** — parent-ADR follow-on; v3+.
- **Trust-policy TOML** — reserved at exit codes 78/79; v3+.
- **SPDX 3.0** — no Rust parser; deferred until one exists or we write one.
- **CycloneDX 1.6** — `cyclonedx-bom` 0.8.1 tops out at 1.5; 1.6 bytes → `sbom_unsupported_format`.
- **In-toto predicate deep parse** — unwrap DSSE envelope + classify predicate type, do not validate predicate schema beyond type discrimination.
- **Notation (JWS) verification** — still no Rust library.
- **DSSE attestation verification** — `sigstore-rs` 0.13 gap; waiting for 0.14.
- **GitHub Attestations API** — not in the OCI graph; `gh attestation verify` is the right tool.
- **Referrer pagination** — referrer lists are small in practice (<10 entries per subject per research); v3+ if load-bearing.
- **`ocx clean --referrer-cache`** — deferred; a `find ~/.ocx/blobs/*/.referrers/ -mtime +N -delete` one-liner is documented instead.
- **Auto-verify on install** — unchanged from Slice 1: never a silent default.

## References

- Slice 1 ADR: [`adr_oci_referrers_signing_v1.md`](./adr_oci_referrers_signing_v1.md)
- Slice 1 plan: [`../state/plans/plan_slice1_sign_and_verify.md`](../state/plans/plan_slice1_sign_and_verify.md)
- Slice 1 PRD: [`prd_oci_referrers_signing_v1.md`](./prd_oci_referrers_signing_v1.md)
- Slice 1 PR-FAQ: [`pr_faq_oci_referrers_signing_v1.md`](./pr_faq_oci_referrers_signing_v1.md)
- Parent ADR: [`adr_oci_artifact_enrichment.md`](./adr_oci_artifact_enrichment.md)
- Superseded ADR (historical): [`adr_oci_referrers_discovery.md`](./adr_oci_referrers_discovery.md) — Status to be set to "Superseded by adr_oci_referrers_discovery_v2.md".
- Meta-plan: [`../state/plans/meta-plan_oci_referrers_signing.md`](../state/plans/meta-plan_oci_referrers_signing.md)
- Research:
  - [`research_cosign_sigstore_notation.md`](./research_cosign_sigstore_notation.md) §"Dual-Format Detection Plan", §3 "SBOM Parsing", §"What `ocx sbom` Should Display"
  - [`research_verify_cli_patterns.md`](./research_verify_cli_patterns.md)
  - [`research_oci_referrers_2026.md`](./research_oci_referrers_2026.md)
- Codex review: [`codex_review_plan_oci_referrers.md`](./codex_review_plan_oci_referrers.md) — all 9 findings inherited from Slice 1 baking
- Spec:
  - [OCI Distribution Spec v1.1](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) — Referrers API, fallback-tag schema, `OCI-Filters-Applied`
  - [Sigstore Bundle v0.3 spec](https://docs.sigstore.dev/about/bundle/) — for interop mention
  - [CycloneDX 1.5 spec](https://cyclonedx.org/docs/1.5/json/) — SBOM format
  - [SPDX 2.3 spec](https://spdx.github.io/spdx-spec/v2.3/) — SBOM format
- Rules:
  - [`quality-rust.md`](../rules/quality-rust.md) + [`quality-rust-errors.md`](../rules/quality-rust-errors.md) + [`quality-rust-exit_codes.md`](../rules/quality-rust-exit_codes.md) — error patterns + exit codes
  - [`subsystem-oci.md`](../rules/subsystem-oci.md) — OciTransport trait, ChainMode, `Result<Option<T>>` convention
  - [`subsystem-cli.md`](../rules/subsystem-cli.md) + [`subsystem-cli-api.md`](../rules/subsystem-cli-api.md) + [`subsystem-cli-commands.md`](../rules/subsystem-cli-commands.md) — Printable trait, single-table rule, typed enums
