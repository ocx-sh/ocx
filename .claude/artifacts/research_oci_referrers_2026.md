# Research: OCI Referrers API — 2026 Domain Snapshot

**Date:** 2026-04-19
**Scope:** Phase 2 research for ocx verify / ocx sbom.
**Axis:** OCI spec + registry compat + verification threat model.

## TL;DR

- **GHCR still has no Referrers API** as of April 2026. The community discussion (#163029, opened June 2025) remains open with no roadmap or timeline from GitHub. A no-fallback stance on the read path means `ocx verify` silently returns "no signatures" for all cosign-signed packages on the most widely used free registry. This is the single biggest practical risk to the feature's utility.
- **The fallback tag scheme is alive and necessary** — Docker Hub also lacks the Referrers API natively, and cosign v3 writes both Referrers API referrers AND `sha256-{digest}` fallback tags simultaneously to maximize compatibility. OCX must read both paths on the read side.
- **cosign fallback tag bug #4641 is confirmed open as of January 2026** — `artifactType` is set to `application/vnd.oci.empty.v1+json` instead of the real type in fallback index entries on registries without the Referrers API. A client parser that filters by `artifactType` on fallback tag content will silently miss all affected signatures.
- **cosign v3 made OCI 1.1 the default** — container signatures are stored as OCI Image 1.1 referring artifacts. `COSIGN_EXPERIMENTAL` is no longer needed. Several commands (download, verify-attestation, attach) still had gaps as of August 2025 (issue #4335), with `--experimental-oci11` as a transitional flag.
- **Per-platform descent is mandatory** — the subject for a signature is the per-platform `ImageManifest` digest, not the `ImageIndex` digest. Querying referrers against the `ImageIndex` digest returns empty results.
- **ECR has a live bug with OCI 1.1 referrer manifest pushes** (issue #2783, 405 on `oras copy -r` as of March 2026), despite the June 2024 GA announcement. Reads appear to work; the bug affects cross-registry copy workflows.
- **Verification threat model gap**: `ocx verify` at the minimum viable level proves "a signature blob exists and is cryptographically self-consistent." It does NOT prove the signer had authority to sign this package, nor detect a compromised OIDC identity. Policy checks (expected issuer, expected subject identity) must be explicit and user-configurable.

---

## 1. OCI Distribution Spec 1.1 Referrers

### 1.1 Referrers API Endpoint

The OCI Distribution Specification 1.1.0 (released 2024-02-15) defines end-12a:

```
GET /v2/{name}/referrers/{digest}?artifactType={filter}
```

The response is an `application/vnd.oci.image.index.v1+json` document (an OCI ImageIndex) where each manifest descriptor represents one referrer whose `subject` field points to `{digest}`. The registry MUST NOT return `404 Not Found` if it supports the endpoint — returning an empty ImageIndex (`{"manifests":[]}`) is correct when no referrers exist.

Registry capability detection: if the endpoint returns `404` or `405`, the registry does not implement the Referrers API. Clients MUST probe for this before assuming native support.

When the registry processes a push of a manifest containing a `subject` field, it MUST respond with the `OCI-Subject: <subject-digest>` header in the push response, signaling to the client that the registry registered the referrer relationship server-side.

**Source:** [opencontainers/distribution-spec spec.md](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) (retrieved 2026-04-19)

### 1.2 Fallback Tag Scheme (`sha256-{digest}`)

For backwards compatibility with registries that do not implement the Referrers API, the spec defines a client-managed tag that mirrors the Referrers API response:

- Take the digest string (e.g., `sha256:aaaa...aaaa`)
- Replace `:` with `-`
- Truncate algorithm to 32 chars, encoded portion to 64 chars
- Replace any characters invalid in OCI tag references with `-`
- Result: `sha256-aaaa...aaaa` (64 hex chars)

The tag points to an OCI ImageIndex structurally identical to what the Referrers API would return. Clients that push a new referrer to a non-API registry must:

1. Pull the current tag (`GET /v2/{name}/manifests/sha256-{digest}`)
2. If it does not exist, start with an empty index
3. Append the new referrer descriptor
4. Push the updated index back (risks a race condition if concurrent writers exist — the spec acknowledges this)

For the **read path**, clients should:
1. Try `GET /v2/{name}/referrers/{digest}` first
2. If `404` or `405`, try `GET /v2/{name}/manifests/sha256-{digest}`
3. Parse both responses identically

**Source:** [opencontainers/distribution-spec spec.md §Referrers Tag Schema](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) (retrieved 2026-04-19)

### 1.3 `OCI-Filters-Applied` Header

The semantics are split between SHOULD and MUST, which matters for client implementation:

- Registry SHOULD support filtering by `artifactType` (filtering support is encouraged, not required)
- When filtering **is** applied, the response MUST include `OCI-Filters-Applied: artifactType`
- If filtering is not supported, the registry returns an unfiltered list — the MUST clause never fires
- Clients must handle both cases: check for `OCI-Filters-Applied` in the response; if absent, filter the returned list client-side

**OCX implication:** Always pass `?artifactType=` to the Referrers API endpoint. Check for `OCI-Filters-Applied` in the response. If the header is absent, apply the filter in the client before presenting results. This is not optional — several production registries return all referrers unfiltered regardless of the query parameter.

**Source:** [opencontainers/distribution-spec spec.md §OCI-Filters-Applied](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) (retrieved 2026-04-19)

---

## 2. Registry Compat Matrix (2026-04)

| Registry | Referrers API | Since | Known Issues | Cosign Default | Notes |
|---|---|---|---|---|---|
| **Docker Hub** | No | — | No native Referrers API; registry is Distribution v1.0.1 compliant, not v1.1 | Writes `sha256-{digest}` fallback tags | Cosign fallback bug #4641 applies — `artifactType` wrong in fallback index entries |
| **GHCR** | No | — | No Referrers API, no roadmap; community discussion #163029 open since Jun 2025; GitHub Attestations API is a proprietary alternative | Writes `sha256-{digest}` fallback tags | OCX `verify` returns empty on GHCR without fallback tag read |
| **AWS ECR (private + public)** | Yes (with bugs) | Jun 2024 | Issue #2783: 405 on `oras copy -r` with OCI 1.1 referrer manifests as of Mar 2026 — Notation 1.2+ required for GA-level compat; older Notation used workarounds | Uses Referrers API when available | GA announced Jun 2024; private and ECR Public both covered; replication of referrers supported |
| **Azure ACR** | Yes | Jan 2023 (RC), Jun 2024 (stable OCI 1.1 certified) | CMK-encrypted registries fall back to tag schema | Uses Referrers API | First cloud registry to claim OCI 1.1 conformance; ORAS verified compatible |
| **Google Artifact Registry** | Yes | Oct 2024 | No specific bugs confirmed; GCR (legacy) is shut down since Mar 2025; use GAR | Uses Referrers API | GAR Docker format repos support OCI 1.1 as GA; release notes confirmed Oct 3, 2024 |
| **Harbor** | Yes | v2.9 (Aug 2023, RC) | Issue #22592: cosign OCI 1.1 referrer signatures displayed as `UNKNOWN` type in Harbor UI as of v2.14.0 | Uses Referrers API | Full spec support; `oras discover` compatible; ongoing cosign/Harbor interop edge cases |
| **JFrog Artifactory** | Yes | v7.90.1 | Requires Cosign 2.0+ for full compat | Uses Referrers API | Full OCI 1.1 conformance announced; dedicated Referrers REST API docs available |
| **Zot** | Yes | Early (pre-v1.0, built OCI-native) | None known | Uses Referrers API | Built from the ground up on OCI Distribution Spec; reference-quality compliance; CNCF Sandbox project |
| **registry:2 (distribution)** | ~~Yes~~ **No (corrected 2026-07-09, #195)** | — | — | Tag-schema fallback only | **CORRECTION (2026-07-09, #195):** empirically FALSE. distribution `registry:2` AND `registry:3` (3.1.1) do NOT serve `GET /v2/<name>/referrers/<digest>` (Go returns `404 page not found`) and omit the `OCI-Subject` header on push — they rely on the referrers tag-schema fallback. The OCX acceptance harness therefore uses **zot**, not registry:2. Do not re-cite the original "Yes" row. |
| **Red Hat Quay / Quay.io** | Yes | Nov 2025 | None known as of 2026-04 | Uses Referrers API | Announced Nov 25, 2025; Quay 3.12 release; supply-chain tooling (Konflux) integration |

**Key takeaway:** 8 of 10 major registries support the Referrers API natively. The two that do not — Docker Hub and GHCR — together represent the majority of publicly hosted package usage for open-source projects. This makes the fallback tag read path practically necessary for `ocx verify` to be useful.

**Sources:**
- [AWS ECR OCI 1.1 GA blog post](https://aws.amazon.com/blogs/opensource/diving-into-oci-image-and-distribution-1-1-support-in-amazon-ecr/) (Jun 2024)
- [Azure ACR OCI 1.1 support announcement](https://techcommunity.microsoft.com/blog/appsonazureblog/announcing-support-of-oci-v1-1-specification-in-azure-container-registry/4177906) (Jun 2024)
- [Google Artifact Registry release notes](https://docs.cloud.google.com/artifact-registry/docs/release-notes) (Oct 2024)
- [JFrog OCI 1.1 conformance announcement](https://jfrog.com/blog/full-conformance-to-oci-v1-1/) (2024)
- [Harbor v2.9 release notes](https://goharbor.io/blog/harbor-2.9/) (Aug 2023)
- [Quay.io Referrers API announcement](https://www.redhat.com/en/blog/announcing-open-container-initiativereferrers-api-quayio-step-towards-enhanced-security-and-compliance) (Nov 2025)
- [GHCR community discussion #163029](https://github.com/orgs/community/discussions/163029) (Jun 2025, still open)
- [ECR referrer manifest bug #2783](https://github.com/aws/containers-roadmap/issues/2783) (Mar 2026, open)

---

## 3. Cosign Fallback Tag Bugs

### Bug #4641 — `artifactType` Missing from Fallback Tag Index Entries

**Status:** Open as of 2026-01-15 (most recent activity). No fix merged.

**Affected versions:** cosign v3.0.4 and earlier (the go-containerregistry upstream fixes are proposed but stalled: `google/go-containerregistry#1931` and `#2068`).

**Root cause:** When cosign pushes to a registry without the Referrers API, it delegates to go-containerregistry to build the fallback index. go-containerregistry does not pull up the `artifactType` from the manifest into the fallback index descriptor. Instead, the descriptor in the fallback tag index contains `"artifactType": "application/vnd.oci.empty.v1+json"` (the config media type fallback) rather than the real type (e.g., `application/vnd.cosign.signature.v1`).

**Annotations are also absent** from the fallback tag index entries.

**OCX defensive parsing requirements:**

1. When reading a fallback tag (`sha256-{digest}`) index, do NOT filter by `artifactType` at the index level — every entry may have the wrong type.
2. Fetch each referrer manifest individually and check `artifactType` in the manifest body itself.
3. Alternatively: accept all entries from the fallback index, then classify by fetching each manifest and inspecting its `config.mediaType` and `artifactType` fields.
4. This adds N+1 round trips (one per referrer) in the fallback path — acceptable since fallback tag lists are typically small (<10 entries).

**Source:** [sigstore/cosign issue #4641](https://github.com/sigstore/cosign/issues/4641) (opened 2026-01-15)

### Bug GHSA-whqx-f9j3-ch6m — Rekor Entry Bypass

**Status:** Fixed in cosign v2.6.2 and v3.0.4.

**Summary:** A crafted bundle could pass `cosign verify` even if the embedded Rekor entry did not reference the artifact's digest, signature, or public key. The bundle signature and key were not compared to the Rekor entry in all code paths. Previously fixed in GHSA-8gw7-4j42-w388, then regressed.

**OCX implication:** OCX should specify a minimum cosign version requirement (v2.6.2 / v3.0.4) in its verify documentation. If OCX is calling cosign as a subprocess, pin the version. If OCX is implementing verification directly using Rust libraries, this upstream bug does not apply, but the same verification check (bundle signature and key MUST match Rekor entry) is an invariant to implement.

**Source:** [sigstore/cosign GHSA-whqx-f9j3-ch6m](https://github.com/sigstore/cosign/security/advisories/GHSA-whqx-f9j3-ch6m)

### Command Coverage Gap (Issue #4335)

As of August 2025, cosign's OCI 1.1 Referrers API support was incomplete across commands. Commands with confirmed OCI 1.1 support: `sign`, `verify`, `tree`, `attach sbom`. Commands with confirmed gaps (still using tag-based storage): `download attestation`, `download signature`, `verify-attestation` (partial), `attach attestation`.

**OCX implication:** Signatures pushed by `cosign sign` after cosign v3 will use the Referrers API path (on supporting registries) or the fallback tag (on GHCR/Docker Hub). Attestations pushed by `cosign attest` may still use the legacy `.att` tag convention depending on version. OCX's SBOM lookup should check both the referrer graph and the `sha256-{digest}.att` tag pattern.

**Source:** [sigstore/cosign issue #4335](https://github.com/sigstore/cosign/issues/4335) (opened Aug 2025)

---

## 4. Per-Platform Referrer Descent

OCX packages are multi-platform: a tag points to an `ImageIndex`, which contains per-platform `ImageManifest` entries. The OCI `subject` field in a signature referrer MUST point to the per-platform `ImageManifest` digest, not the `ImageIndex` digest — because the signature covers the specific binary, and a linux/amd64 binary is cryptographically distinct from darwin/arm64.

**Correct resolution pseudocode (2 round trips, not 3):**

```
fn resolve_referrers(repo, tag_or_digest, platform, artifact_type) -> Vec<Referrer>:

    // Round trip 1: pull the index (or manifest if already platform-specific)
    manifest = oci_client.pull_manifest(repo, tag_or_digest)

    platform_digest = match manifest:
        ImageIndex(index) =>
            // Platform selection is already implemented in OCX via PlatformKey
            descriptor = index.manifests
                .find(|m| matches_platform(m.platform, platform))
                .ok_or(NoPlatformFound)?
            descriptor.digest                   // no additional round trip needed
        ImageManifest(_) =>
            // already a single-platform manifest
            tag_or_digest

    // Round trip 2: query referrers for the platform-specific digest
    referrers = query_referrers(repo, platform_digest, artifact_type)
    return referrers

fn query_referrers(repo, digest, artifact_type) -> Vec<Referrer>:
    // Try Referrers API first
    response = GET /v2/{repo}/referrers/{digest}?artifactType={artifact_type}

    if response.status == 200:
        index = parse_image_index(response.body)
        if response.headers["OCI-Filters-Applied"] contains "artifactType":
            return index.manifests   // already filtered server-side
        else:
            return index.manifests.filter(|m| m.artifact_type == artifact_type)

    if response.status in [404, 405]:
        // Fallback to tag schema
        fallback_tag = digest.replace(":", "-")  // "sha256:abc..." → "sha256-abc..."
        tag_response = GET /v2/{repo}/manifests/{fallback_tag}
        if tag_response.status == 404:
            return []   // no signatures, not an error
        index = parse_image_index(tag_response.body)
        // Bug #4641: artifactType in fallback entries may be wrong
        // Do NOT filter by artifactType here — fetch each manifest individually
        candidates = index.manifests
        return candidates.filter_map(|desc| {
            manifest = GET /v2/{repo}/manifests/{desc.digest}
            if manifest.artifact_type == artifact_type: Some(manifest)
            else: None
        })
```

**Key points:**
- The `ImageIndex` contains the platform descriptor's `digest` inline — no separate round trip to pull the per-platform manifest just to get its digest.
- Total round trips on the happy path (Referrers API + no fallback): 2 (index pull + referrers query).
- Total round trips on the fallback path: 2 + N where N = number of entries in the fallback tag index (typically 1–3 for most packages).
- The 3-round-trip scenario only occurs if `tag_or_digest` is already a platform manifest digest (bypassing the index) AND the fallback tag has multiple entries to classify.

---

## 5. Verification Threat Model

### What `ocx verify` Proves at Different Levels

| Level | What it proves | Implementation cost | Required infrastructure |
|---|---|---|---|
| **L0 — Syntactic validity** | Signature blob exists; signature is well-formed (parseable) | Trivial | None |
| **L1 — Cryptographic binding** | The signature was produced by someone who held the private key corresponding to the embedded certificate; the signed payload matches the artifact digest | Low (use `sigstore-rs` or invoke `cosign verify`) | None (offline if using embedded cert) |
| **L2 — Rekor inclusion** | The signing event is in the public Rekor transparency log; inclusion proof verified | Medium | Network (Rekor API) or pre-fetched bundle |
| **L3 — Fulcio cert chain** | The certificate chains to the Fulcio CA root; the identity (OIDC subject + issuer) is embedded in the cert and auditable | Medium | TUF root metadata (Sigstore TUF) |
| **L4 — Policy match** | The embedded identity matches a declared expected issuer + subject (e.g., `issuer: https://token.actions.githubusercontent.com`, `subject: https://github.com/owner/repo/.github/workflows/...`) | High (requires user policy config) | Policy specification per package |

### What OCX v1 MVI Should Prove

**Recommendation: L1 + L3 (cryptographic binding + Fulcio cert chain) with Rekor inclusion as optional enhancement.**

Rationale:
- L0 (syntactic only) is not meaningful — it says nothing about who signed
- L1 without L3 allows any self-signed key to pass verification — trivially forgeable
- L1 + L3 together prove the signer had a valid Fulcio-issued certificate at signing time and that the OIDC identity is embedded and auditable — this is the minimum that prevents trivial forgery
- L2 (Rekor) requires network connectivity and adds latency; Sigstore bundles embed the inclusion proof, making offline Rekor verification possible if the bundle was pre-fetched — include in MVI but make it gracefully degrade
- L4 (policy) is essential for real security (without it, any GitHub Actions workflow on any repo can sign), but requires user-visible configuration that is scope for v2

### What It Explicitly Does NOT Prove

- **Authority to sign:** A cosign signature from `github.com/attacker/fork` is cryptographically valid. Without L4 policy matching, `ocx verify` passes for adversarially-signed packages. This MUST be documented.
- **OIDC account compromise:** If the GitHub account used in CI was compromised at signing time, the signature is valid. Sigstore's threat model explicitly does not protect against this.
- **Registry integrity:** The referrer graph itself is not signed. A registry operator could theoretically insert or remove referrer entries.
- **Freshness:** The signature may have been valid at signing time but the package may have been modified in transit (though content-addressing mitigates this for well-implemented clients).

### Recommended MVI Output

```
ocx verify cmake:3.28 --platform linux/amd64

  Signature found: 1
  Artifact digest: sha256:aaa...

  [PASS] Cryptographic binding: signature verified against artifact digest
  [PASS] Certificate: issued by Fulcio (https://fulcio.sigstore.dev)
  [PASS] Identity: sigstore@ocx.sh (OIDC: https://accounts.google.com)
  [PASS] Rekor inclusion: log index 123456789
  [WARN] No policy configured: signature is valid but signer authority not verified
         Use --expected-issuer and --expected-identity to enforce policy
```

---

## 6. Caching Policy

### Problem

- Docker Hub enforces hard rate limits: 10 pulls/hour anonymous, 40 pulls/hour authenticated (as of Dec 2024); paid plans get unlimited. Referrer lookups consume the same quota as manifest pulls.
- Referrer indexes are content-addressed (the fallback tag index is mutable, but the Referrers API response can be cached by digest).
- Referrer index sizes are typically small: 1–5 entries for most packages (one signature per platform build, optionally one SBOM).

### Proposed Caching Layout

Extend the existing `~/.ocx/` layout (already established in the product context and ADR):

```
~/.ocx/
├── blobs/{registry}/{sha256-prefix}/{sha256-digest}
│   # Existing: raw OCI blobs (manifests, image layers)
│   # New: referrer index JSON files, cached by their own digest
│   # Key: the digest of the referrer index response body
│   # TTL: referrer indexes should have short TTL (1h) or be invalidated on verify
│
├── referrers/{registry}/{repo}/{subject-digest}.json
│   # Cached referrer index keyed by the subject digest
│   # This is NOT content-addressed (the list can change as new referrers are pushed)
│   # Must have explicit TTL or be cache-busted on verify-command execution
│   # Format: the raw ImageIndex JSON as returned by the Referrers API
│
└── referrers/{registry}/{repo}/{subject-digest}.meta
    # Metadata: fetched_at, registry_supports_api (bool), fallback_tag_used (bool)
    # Used to decide whether to re-probe the API or skip straight to tag schema
```

### Caching Rules

1. **Referrer indexes are mutable** — a new signature can be pushed at any time. Cache duration should be short (default: 1 hour for `ocx verify`; 24 hours for non-interactive contexts like CI).
2. **Individual referrer blobs are immutable** — once fetched and content-addressed, cache indefinitely (same policy as image layers).
3. **Registry capability detection** — cache the result of probing the Referrers API endpoint per registry per session. Persist across sessions with a 7-day TTL to avoid probing on every invocation.
4. **Rate limit handling** — on 429, back off and retry with `Retry-After` header; cache the rate-limit state to avoid hammering. Prefer a warm cache hit over a network call on Docker Hub.
5. **`--no-cache` flag** — `ocx verify` should always support bypassing the cache for security-sensitive workflows.

### Cache Invalidation Strategy

- On `ocx verify <ref>`: always re-fetch the referrer index (bypass the mutable cache), but use the blob cache for individual referrer manifests whose digest is already cached.
- On `ocx sbom <ref>`: same as verify.
- Automated CI contexts (non-interactive): can use the cached referrer index for 24 hours to avoid rate limits.

---

## 7. Answer: Issue #24 Open Question 3 — Tag Fallback Stance

**Question:** Should OCX hold the "no tag fallback" stance established in the push-path ADR for the read path of `ocx verify` and `ocx sbom`?

### The Stakes

Without read-side fallback tag support:
- `ocx verify cmake:3.28` on a GHCR-hosted package returns "no signatures found" for every package signed with cosign, because cosign writes `sha256-{digest}` fallback tags that OCX would ignore.
- This is not a corner case — GHCR is the default registry for GitHub Actions-published packages, which is OCX's primary automation target.
- Docker Hub packages are also affected.
- The failure mode is **silent** unless OCX explicitly detects "registry lacks Referrers API" and surfaces a warning distinguishing "no signatures" from "cannot check signatures."

Without the fallback, `ocx verify` is not useful for the two most common registries in the target user's environment.

### Recommendation: Split the Stance

**Push path: hold the line.** Writing `sha256-{digest}` fallback tags when publishing OCX supply-chain artifacts adds writer complexity (concurrent update races, cleanup, permanent second code path). OCX controls its push path; it should not write fallback tags and should clearly document that OCX-native signatures/SBOMs require a Referrers API-capable registry.

**Read path: implement fallback tag reading.** The read path does not have writer concerns — it is read-only. The only cost is additional implementation complexity (~50 lines of Rust), and the benefit is compatibility with the entire cosign-signed ecosystem on GHCR and Docker Hub. The ADR's cost justification ("not justified for a transitional compatibility shim") applies to the write path; it does not apply symmetrically to reading.

### Conditions on Read-Side Fallback

1. **Fallback is transparent to the user** — OCX should log at debug level which path was used, but not surface "warning: using fallback tags" unless the user asks for verbose output.
2. **Bug #4641 defense is required** — The fallback path MUST fetch each candidate referrer manifest individually to determine `artifactType` (see Section 3). Do not trust `artifactType` in the fallback index entries.
3. **Distinguish "no signatures" from "API unsupported"** — at normal verbosity, when the Referrers API is unavailable AND the fallback tag does not exist, output: "No signatures found (registry does not support Referrers API; checked fallback tag sha256-{digest})."
4. **Registry capability cache** — after detecting that a registry lacks the Referrers API, cache this per-registry for 7 days to avoid repeated probes.
5. **Future-proof** — when GHCR adds Referrers API support, OCX automatically switches to the native path (probe first, fall back only on 404/405). No code changes or user action needed.

### Rejected Alternative: "Explicit Flag"

One might argue for `ocx verify --fallback-tags` as an opt-in. This is rejected: it puts the burden on the user to know which registry they are on, which is an implementation detail OCX should abstract. The probe-first-then-fallback logic is trivially automatable and is what every other OCI client (oras, crane, cosign itself) does.

---

## Citations

- [OCI Distribution Spec v1.1 — spec.md](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) — referrers endpoint, OCI-Filters-Applied header, fallback tag schema; retrieved 2026-04-19
- [OCI Image/Distribution Specs v1.1 Release Announcement](https://opencontainers.org/posts/blog/2024-03-13-image-and-distribution-1-1/) — release date confirmation (2024-02-15), feature highlights
- [AWS ECR OCI 1.1 Support (AWS Open Source Blog)](https://aws.amazon.com/blogs/opensource/diving-into-oci-image-and-distribution-1-1-support-in-amazon-ecr/) — ECR GA June 2024, referrers replication, Notation 1.2 requirement
- [ECR referrer manifest bug #2783](https://github.com/aws/containers-roadmap/issues/2783) — 405 on `oras copy -r` with OCI 1.1 manifests, open as of Mar 2026
- [Azure ACR OCI 1.1 Announcement](https://techcommunity.microsoft.com/blog/appsonazureblog/announcing-support-of-oci-v1-1-specification-in-azure-container-registry/4177906) — certified conformance, Jun 2024; early RC support Jan 2023; CMK exception
- [Google Artifact Registry Release Notes](https://docs.cloud.google.com/artifact-registry/docs/release-notes) — OCI 1.1 GA October 3, 2024
- [JFrog OCI 1.1 Conformance Blog](https://jfrog.com/blog/full-conformance-to-oci-v1-1/) — Referrers API from Artifactory v7.90.1
- [JFrog Referrers REST API Docs](https://jfrog.com/help/r/jfrog-artifactory-documentation/use-referrers-rest-api-to-discover-oci-references) — usage details
- [Harbor v2.9 Release Notes](https://goharbor.io/blog/harbor-2.9/) — Referrers API support from Aug 2023
- [Harbor cosign/OCI 1.1 issue #22592](https://github.com/goharbor/harbor/issues/22592) — signature type displayed as UNKNOWN in Harbor UI
- [Quay.io Referrers API Announcement](https://www.redhat.com/en/blog/announcing-open-container-initiativereferrers-api-quayio-step-towards-enhanced-security-and-compliance) — GA November 25, 2025
- [GHCR Referrers API community discussion #163029](https://github.com/orgs/community/discussions/163029) — no support, no roadmap, open Jun 2025
- [cosign issue #4641 — fallback tag artifactType bug](https://github.com/sigstore/cosign/issues/4641) — opened Jan 15, 2026; unfixed; go-containerregistry upstream stalled
- [cosign issue #4335 — incomplete OCI 1.1 command coverage](https://github.com/sigstore/cosign/issues/4335) — opened Aug 2025; partial coverage of download/attach/verify-attestation commands
- [cosign GHSA-whqx-f9j3-ch6m — Rekor entry bypass vulnerability](https://github.com/sigstore/cosign/security/advisories/GHSA-whqx-f9j3-ch6m) — fixed in v2.6.2 / v3.0.4
- [Chainguard — Building towards OCI v1.1 support in cosign](https://www.chainguard.dev/unchained/building-towards-oci-v1-1-support-in-cosign) — dual-path behavior (API + fallback), prototype description
- [Sigstore Threat Model](https://docs.sigstore.dev/about/threat-model/) — what verification proves and does not prove; OIDC compromise not mitigated; policy gap
- [Sigstore Security Model](https://docs.sigstore.dev/about/security/) — Fulcio CT log, Rekor TUF root, certificate lifecycle
- [Docker Hub Pull Rate Limits](https://docs.docker.com/docker-hub/usage/pulls/) — current limits (10/hr anonymous, 40/hr authenticated, unlimited for paid plans from Apr 2025)
- [OCI Referrers API on Zot](https://github.com/project-zot/zot) — OCI-native registry, full referrers support from early versions
