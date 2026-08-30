# Research — cross-registry promotion of signatures, attestations and SBOMs (2026)

Persisted by the orchestrator: `worker-researcher` has no Write tool in this repo, so the
worker returned inline and this file is the orchestrator's capture. (Known limitation,
recorded in `.agents/memory/hex.md`.)

- **Date:** 2026-08-30
- **Axis:** cross-registry promotion of signed artifacts — state of the art
- **Consumer:** [ocx-sh/ocx#376](https://github.com/ocx-sh/ocx/issues/376), WP-10 of
  `plan_issue_sweep_2026-08-30.md`

## Recommendation (as delivered)

Add a **source-side read-only fallback**: when `--include-referrers` finds nothing via the
OCI Referrers API + OCX's own `<algo>-<hex>` fallback tag, additionally probe the three
legacy cosign sidecar tags (`sha256-<hex>.sig`, `.att`, `.sbom`) at the source, union by
digest, and copy through the existing verbatim-manifest path. A large currently-deployed
population of signed images is discoverable *only* that way (cosign v2 default; cosign v3
against a non-referrers registry). Two long-lived open issues in mainstream tools show
this exact gap causing silent signature loss in production today.

The worker further recommended **never writing legacy tag names at the destination**,
citing the lost-update race in
[go-containerregistry#2205](https://github.com/google/go-containerregistry/issues/2205).

> **Orchestrator dissent, adopted as decision D4 in the plan.** That race is specific to
> the OCI **fallback index tag** — one `<algo>-<hex>` index accumulating all referrers,
> updated read-modify-write with no `If-Match`. A cosign per-type tag
> (`sha256-<hex>.sig`) is a single leaf manifest PUT wholesale: nothing to merge, no race.
> The two mechanisms were conflated. The alternative this implies — re-homing a sidecar as
> a proper referrer — is impossible without reconstruction, since a cosign
> `.sig`/`.att`/`.sbom` manifest declares neither `artifactType` nor `subject`, and
> reconstruction is exactly what corrupts signatures in cosign#4207. **OCX copies sidecars
> verbatim under the same tag name, and still never writes the fallback index tag.**

## Tool comparison

| Tool | Copies referrers | Copies legacy sidecar tags | Flag | Notes |
|---|---|---|---|---|
| `cosign copy` | Only via legacy tag walk | Yes — its whole job | `--only=sig,att,sbom` | **Deprecated in the current v3 source**; its own notice points at `oras copy -r`. Not referrers-API aware. |
| `oras cp` | Yes | No | `-r` / `--recursive` | Walks the referrer graph via the API or the OCI fallback-tag schema (`--from/--to-distribution-spec=v1.1-referrers-tag`). Blind to cosign's pre-OCI-1.1 per-type tags. |
| `crane cp` | **No** | No | none exists | Pure manifest+tag copy, no signature awareness at all. |
| `regctl image copy` | Yes | Partially, **by accident** | `--referrers`, `--digest-tags` | `--digest-tags` globs `sha256-<digest>.*`, which incidentally catches cosign's legacy tags. Only tool of the four with any legacy reach. |
| `skopeo copy` | **No** | **No** | none | skopeo#2061 asks for it, open since 2023. |

**No tool does capability negotiation** beyond an explicit flag pair. None detects "source
has referrers, destination doesn't" and degrades mid-copy. OCX's fail-up-front
`ensure_target_serves_referrers` is the safer of the two patterns seen in the wild.

## Deprecation state

- Cosign v3 defaults to OCI-referrers signing; legacy `.sig`/`.att`/`.sbom` tags are the
  fallback, not removed.
- `cosign attach sbom` / `--attachment sbom` deprecated **2024-02-22**, still present in
  v3.x, **removed only in v4** (unreleased as of 2026-08).
- `cosign copy` deprecated in the current v3 source; exact version UNCONFIRMED (found by
  source inspection, doc page 404s).
- OCI 1.1 Referrers API + official fallback tag schema ratified **2024-03-13**. Algorithm
  segment truncated to 32 chars, encoded segment to 64 — a different shape from cosign's
  three separate per-type leaf tags.
- Cosign's own fallback-tag *write* is currently non-compliant against older registries;
  two fix PRs stalled (cosign#4641, Jan 2026).

## Registry capability matrix

| Registry | Referrers API | Fallback tag needed | Confidence |
|---|---|---|---|
| GHCR | Yes (read+write) | No | Moderate |
| Quay.io | Write-only — does not return referrers on read | Yes, on read | UNCONFIRMED currency |
| Amazon ECR | Yes (added 2024-06) but still 405s on some spec-valid referrer manifests | Partial | Mixed |
| Azure ACR | Yes; CMK-encrypted registries fall back | Only for CMK repos | Good |
| JFrog Artifactory | Yes, since 7.90.1 | No | Good |
| Harbor | Yes for direct push/pull; **replication does not traverse referrers** | Yes, for replicated copies | Good |
| Docker Hub | UNCONFIRMED for 2026 (last statement 2022) | Assume yes | Weak |
| Google Artifact Registry | UNCONFIRMED | Assume yes | Weak |
| Sonatype Nexus | UNCONFIRMED | Assume yes | None |

## Known failure reports

| Source | What broke |
|---|---|
| [skopeo#2061](https://github.com/containers/skopeo/issues/2061) (Aug 2023, open) | `skopeo copy`/`sync` silently drops cosign sidecar signatures; mirror unverifiable |
| [cosign#4207](https://github.com/sigstore/cosign/issues/4207) (May 2025, PR open) | Digest preserved by `--preserve-digests`, but the **signature payload itself** altered on re-attach → ASN.1 error on verify |
| [harbor#23210](https://github.com/goharbor/harbor/issues/23210) (May 2026, open) | Harbor replication doesn't walk the Referrers API; signatures stay behind silently. Direct `oras copy -r` between the same instances works |
| [containers-roadmap#2783](https://github.com/aws/containers-roadmap/issues/2783) (Mar 2026, open) | ECR 405s on spec-valid referrer manifests during `oras copy -r`; identical push succeeds on ACR |
| [go-containerregistry#2205](https://github.com/google/go-containerregistry/issues/2205) | Concurrent writers to a fallback **index** tag lose each other's entries (no ETag/If-Match) |
| [kubernetes.dev, Jun 2026](https://www.kubernetes.dev/blog/2026/06/05/image-signature-routing/) | `registry.k8s.io` deleted a whole signature-replication pipeline (−1200 LOC) by routing signature *requests* upstream instead of copying signatures to 22 backends every 2h |

## Pitfalls, each with its consequence for OCX

- **Verbatim or nothing.** cosign#4207 is a reconstruct-not-copy bug. OCX's
  `leaf_manifest_bytes_survive_the_copy_verbatim` already pins the right discipline —
  extend it to sidecar manifests; never parse and re-serialize one.
- **`subject` needs no rewrite.** The leaf digest is preserved, so every referrer's
  `subject` stays valid at the destination for free.
- **Key off the digest, never a mutable tag.** Legacy tags are named for the subject
  digest; an orphaned tag surviving a retag is a known cosign gotcha. A sidecar whose
  payload doesn't match the requested digest is *absent*, not an error.
- **Round trips.** Three extra GETs per leaf if probed unconditionally. Probe only when
  primary discovery is empty.
- **Partial failure.** Harbor's gap is the exact silent-completion failure OCX's PKG-11
  rule already forbids. A listed-but-unfetchable sidecar fails the copy.
- **Capability probes are advisory.** ECR and Quay both claim support and still reject or
  hide specific manifests. Keep the per-referrer fetch-and-verify backstop.
- **Fallback-tag collision.** sha256/384/512 never collide under the 32/64-char
  truncation, but don't assume that for a future longer digest algorithm.

## Sources

[cosign#4335](https://github.com/sigstore/cosign/issues/4335) ·
[cosign#4641](https://github.com/sigstore/cosign/issues/4641) ·
[cosign#4207](https://github.com/sigstore/cosign/issues/4207) ·
[cosign#2755](https://github.com/sigstore/cosign/issues/2755) ·
[cosign#4696](https://github.com/sigstore/cosign/issues/4696) ·
[cosign copy.go](https://github.com/sigstore/cosign/blob/main/cmd/cosign/cli/copy.go) ·
[skopeo#2061](https://github.com/containers/skopeo/issues/2061) ·
[go-containerregistry#2205](https://github.com/google/go-containerregistry/issues/2205) ·
[containers-roadmap#2783](https://github.com/aws/containers-roadmap/issues/2783) ·
[harbor#23210](https://github.com/goharbor/harbor/issues/23210) ·
[AWS ECR OCI 1.1 blog](https://aws.amazon.com/blogs/opensource/diving-into-oci-image-and-distribution-1-1-support-in-amazon-ecr/) ·
[JFrog OCI v1.1 conformance](https://jfrog.com/blog/full-conformance-to-oci-v1-1/) ·
[oras cp docs](https://oras.land/docs/commands/oras_cp/) ·
[regctl image copy docs](https://regclient.org/cli/regctl/image/copy/) ·
[crane copy docs](https://github.com/google/go-containerregistry/blob/main/cmd/crane/doc/crane_copy.md) ·
[MS Learn — ACR artifacts](https://learn.microsoft.com/en-us/azure/container-registry/container-registry-manage-artifact) ·
[distribution-spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) ·
[kubernetes.dev signature routing](https://www.kubernetes.dev/blog/2026/06/05/image-signature-routing/)

**Unconfirmed, flagged by the worker:** Google Artifact Registry, Docker Hub (2026) and
Sonatype Nexus referrers status could not be established from any 2025–2026 source. Probe
directly before relying on the matrix rows above.
