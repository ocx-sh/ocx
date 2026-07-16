# Research: GHCR (ghcr.io) as Primary Public Package Storage for OCX

**Date:** 2026-07-12
**Domain:** oci, infrastructure, registry
**Triggered by:** [`adr_public_index_registry_indirection.md`](./adr_public_index_registry_indirection.md) — D1 commits physical storage to `ghcr.io/ocx-contrib/<pkg>`, D11 storage-keying, D15 hosting
**Related:** [`adr_oci_registry_mirror.md`](./adr_oci_registry_mirror.md), [`research_registry_mirror.md`](./research_registry_mirror.md) — the `[mirrors]` config proposed as the primary risk mitigation below

---

## Direct Answer

GHCR is a workable, currently-free primary store for a public OCI package catalog, but it has real, **documented gaps** relative to what a good OCI citizen wants: no `_catalog`, no Referrers API, no native tag immutability, historically flaky `tags/list` pagination (fixed in 2021 but a signal of low investment in spec edges), and a **recurring, multi-month-documented regional CDN degradation pattern** (Fastly-fronted pulls, especially from Europe) plus intermittent `429`s on registry pushes under concurrency. None of this blocks OCX's design — the index-indirection ADR already routes around `_catalog` (D1) and already treats GHCR as "physical storage, not source of truth" — but the reliability findings below argue for treating GHCR the same way OCX already treats any other registry: mirror-able, never a single point of failure for CI-heavy consumers.

## Industry Context & Trends

- **Established**: GHCR as free-for-public-packages container/OCI storage is now the default choice for OSS projects fleeing Docker Hub's Nov 2024/Apr 2025 rate-limit tightening (10 pulls/hr unauthenticated, 40/hr authenticated-free) — see the wave of `docker.io` → `ghcr.io` migration threads (go-gitea, Emby, Nextcloud/AIO all cited GHCR as the escape valve). GHCR itself publishes **no rate limit at all** for public pulls today.
- **Trending**: OCI Distribution Spec 1.1 (referrers API, `subject`/`artifactType`) is being adopted by ECR, ACR, Harbor, Zot, and GHCR's own **GitHub Attestations** feature writes referrer-shaped artifacts — but GHCR's registry endpoint itself has **not** implemented the `/v2/<name>/referrers/<digest>` API. This is a real gap vs. the direction the OCI ecosystem is moving (sigstore/cosign, SLSA provenance, SBOM attachment all lean on referrers).
- **Emerging**: `ORAS` + Zot/`distribution` v3 as a self-hostable, fully-1.1-conformant alternative for teams that want referrers/catalog support without giving up "just an OCI registry" simplicity — worth watching if OCX's own tooling (or `ocx-mirror`) ever wants attestation-style side-artifacts.
- **Declining**: Docker Hub as a viable *unauthenticated* CI pull target — its 2024/2025 rate-limit changes are the direct cause of the GHCR migration wave and are the reason GHCR is attractive to OCX in the first place.

---

## 1. Anonymous/Public Pull — Token Flow & Rate Limits

**Token flow.** GHCR follows the standard OCI/Docker bearer-token dance: an unauthenticated `GET /v2/<owner>/<repo>/manifests/<ref>` returns `401` with a `WWW-Authenticate` challenge pointing at `https://ghcr.io/token?service=ghcr.io&scope=repository:<owner>/<repo>:pull`. For **public** repos this token endpoint issues a valid bearer token with **no credentials required** — genuinely anonymous pull. Requesting `push`/`delete` scope anonymously returns `403 requested access to the resource is denied` — write scopes always require Basic auth (PAT or `GITHUB_TOKEN`) up front. GHCR does not return an expiry in the token response, so long-running clients should re-request rather than trust a TTL. ([toddysm.com](https://toddysm.com/2024/02/12/authenticating-with-oci-registries-github-container-registry-ghcr-implementation/) — GHCR auth flow with concrete curl examples)

**Rate limits.** GitHub's own docs state anonymous access to public images is supported but publish **no numeric rate limit** for it. Multiple community threads (GitHub staff-adjacent and independent) converge on: **no documented/enforced pull rate limit for public GHCR packages today**, only usage-based billing (storage + egress) for *private* packages. This is explicitly called out as provisional — "no rate limit *today*, subject to change." ([github/community#49671](https://github.com/orgs/community/discussions/49671))

**Comparison to Docker Hub** (current, post Dec 2024 tightening):

| | Unauthenticated | Authenticated (free) |
|---|---|---|
| Docker Hub | 10 pulls/hr (60/6hr) | 40 pulls/hr (240/6hr) |
| GHCR (public packages) | No published limit | No published limit |

This differential is the entire reason for the OSS "flee Docker Hub → GHCR" migration wave (go-gitea#33711, Emby community, Nextcloud AIO, Flux CD). ([docs.docker.com/docker-hub/usage/pulls](https://docs.docker.com/docker-hub/usage/pulls/), [gecko.security GHCR guide](https://www.gecko.security/blog/ghcr-github-container-registry-guide))

Caveat for OCX specifically: "no rate limit" is a **behavioral observation, not a contract** — GitHub could add one with no notice beyond what's implied by ToS. Design for it as a soft constraint, not a guarantee (see Risk table).

## 2. Org Packages — Visibility, Push Permissions, Repo Linking

- **Default visibility is private** on first publish; must be explicitly switched to public via package settings (UI) or, if repo-linked, inherited from the linked repository's visibility. ([docs.github.com/packages/working-with-the-container-registry](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry))
- **Push auth**: classic PATs only — **fine-grained PATs do not yet support the Packages endpoint** (confirmed current as of the community feedback-tracking discussion; GitHub Packages docs explicitly say "not yet available through fine-grained tokens"). Required scopes: `read:packages` / `write:packages` / `delete:packages`. ([github/community#52868](https://github.com/orgs/community/discussions/52868), [docs.github.com/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens](https://docs.github.com/en/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens))
- **`GITHUB_TOKEN` in Actions**: publishes fine for packages in the *same* repo's namespace, and repos publishing packages automatically get `admin` on that package — **but only if the package is already linked to the repo**. If a package was previously pushed under a namespace without the `org.opencontainers.image.source` label (or manual link), a later workflow's `GITHUB_TOKEN` can be denied push. This is a real footgun for a multi-repo mirror-publishing setup like `ocx-contrib` (many small mirror repos each publishing one package) — link explicitly, don't rely on first-push auto-link.
- **Org-level control**: package visibility/access settings live at repo-or-org level, not at push time; org policy can restrict who is allowed to make packages public at all (relevant if `ocx-contrib` is a fresh org — check org package-creation policy before automating first-push).

## 3. Immutability / Retention

- **Tags are fully mutable and overwritable** by default — no native "tag immutability" feature exists (it's an open community feature request, not shipped: [github/community#181783](https://github.com/orgs/community/discussions/181783)). Overwriting `tag:X` does not delete the old manifest; the prior content stays reachable **by digest** even after the tag is repointed — same content-addressed guarantee OCX already assumes for lockfile pinning.
- **Deletion**: package versions (tags or fully-untagged manifests) are deletable via UI or REST API (`DELETE /orgs/{org}/packages/container/{name}/versions/{id}`), gated behind `delete:packages` + package-admin. **Public packages with >5,000 downloads cannot be deleted** — a hard API-level restriction, notable if OCX ever needs to yank a bad public release. ([docs.github.com/rest/packages](https://docs.github.com/en/rest/packages/packages?apiVersion=2022-11-28))
- **Grace period**: deleted package versions are recoverable via the restore endpoint for **30 days**, provided the namespace hasn't been reclaimed by a new push in the meantime.
- **No native retention/GC policy** (no Azure-ACR-style "delete untagged manifests after N days"). Untagged/orphaned layers persist until manually cleaned; the ecosystem answer is third-party Actions (`snok/container-retention-policy`, "GHCR Cleaner" on the Marketplace) — not a GitHub-native cron. For OCX's mirror-publish pipeline this means **build-up of untagged manifests from failed/retried pushes is our problem to clean**, not GHCR's to auto-GC.

## 4. OCI Spec Conformance Gaps

| Feature | GHCR status | Evidence |
|---|---|---|
| **Referrers API** (`/v2/<name>/referrers/<digest>`, OCI Distribution 1.1) | **Not implemented.** GHCR accepts `subject`-bearing manifest pushes (e.g. GitHub's own `attest-build-provenance` action) but referrers queries return empty — no `OCI-Subject` header, no registry-side discovery of attached provenance/SBOM by digest alone. | [github/community#163029](https://github.com/orgs/community/discussions/163029) |
| **`_catalog`** | **Effectively unsupported.** Multiple independent threads confirm `/v2/_catalog` is not implemented / "in backlog"; where a 200 is observed it's inconsistent and requires broad auth scope, not usable as a public discovery API. Directly validates the OCX ADR's D1 rationale for building index-indirection instead of relying on `_catalog`. | [github/community#26279](https://github.com/orgs/community/discussions/26279), [github/community#26299](https://github.com/orgs/community/discussions/26299) |
| **`tags/list` pagination** (`Link` header per spec) | Historically broken (no `Link` header returned, breaking `crane` and other pagination-aware clients); GitHub staff confirmed a fix landed **2021-08-16**. Long-fixed, but signals these API edges get low ongoing test coverage — worth a defensive check rather than blind trust. | [github/community#25658](https://github.com/orgs/community/discussions/25658) |
| Manifest media types | Historical bug: GHCR once returned `docker.distribution.manifest.v2+json` instead of the requested `manifest.list.v2+json` (containerd#4520, long since fixed). Currently, some multi-arch images serve `application/vnd.oci.image.index.v1+json` where a consumer expects the Docker manifest-list type, breaking tools like Dependabot that pattern-match content-type. | [containerd#4520](https://github.com/containerd/containerd/issues/4520), [dependabot-core#13383](https://github.com/dependabot/dependabot-core/issues/13383) |

## 5. Reliability / SLA Posture & Egress

- **Billing**: GitHub Packages is **free for public packages** — no storage or bandwidth charge documented. Private packages draw from plan-based storage/transfer allowances; GitHub commits to "at least one month advance notice" before any pricing change, and explicitly carves out that **bandwidth used pulling/pushing inside GitHub Actions workflows stays free regardless of future billing changes** — directly relevant since OCX's own CI (mirror publishing, dogfooding) runs inside Actions. ([docs.github.com/packages/learn-github-packages/introduction-to-github-packages](https://docs.github.com/en/packages/learn-github-packages/introduction-to-github-packages))
- **Size caps**: **10 GB per layer**, **10-minute upload timeout** — hard limits from official docs. Any single-binary artifact under these is fine; a bundled toolchain layer (e.g. a full SDK) should be checked against 10 GB.
- **No published SLA/uptime number** for GHCR specifically (GitHub's general status page covers "Packages" as a component, not a GHCR-specific SLA).
- **Recurring reliability pattern (the most actionable finding of this research):** independent, dated reports spanning **September 2025 → May 2026** describe a **daily, predictable regional slowdown** — GHCR pulls from Europe degrading to 1–3 Mbps around 14:00 CEST, recovering near midnight, while US pulls stay fast. GitHub support attributed at least one instance directly to Fastly: *"There was a disruption with Fastly yesterday that affected some regions in Europe... GHCR sometimes redirects to partner CDNs like Fastly via pkg-containers.githubusercontent.com, so actual byte throughput depends on that CDN's performance."* This confirms GHCR blob delivery is **CDN-fronted and inherits that CDN's regional weather** — the GitHub status page can be green while real-world pull throughput in a region is degraded. ([github/community#173607](https://github.com/orgs/community/discussions/173607))
- **Push-side throttling under concurrency**: separate from pull throttling, `docker/build-push-action` users report intermittent `429 Too Many Requests` from `POST .../blobs/uploads/` when multiple jobs push concurrently, with **no automatic retry** in the action (open feature request, [docker/build-push-action#1194](https://github.com/docker/build-push-action/issues/1194)). Directly relevant to OCX's mirror-publish CI, which will push many packages, potentially in parallel matrix jobs.

## 6. Multi-Arch Image Index Behavior — GHCR-Specific Quirks

- **`buildx`-based attestations pollute the index.** When images are built with `docker buildx build --provenance=true` (the buildx default since v0.10) or SBOM attestations enabled, GHCR's package UI (and some naive index-walking tools) show a spurious **`unknown/unknown`** platform entry in the manifest list — the attestation manifest itself, which has no real platform, gets listed alongside real per-arch entries. Workaround: disable `provenance`/`sbom` in the build step, or have consumers explicitly filter `unknown/unknown` when walking the index. Directly relevant to OCX's `Manifest::ImageIndex` / `Platform` matching (`crates/ocx_lib/src/oci/platform.rs`) — if OCX's own publish path (or `ocx-mirror`) ever uses buildx with attestations to build GHCR-hosted images, `fetch_candidates()` needs to tolerate or filter non-platform entries, not just trust every child manifest carries a real `Platform`. ([github/community#45969](https://github.com/orgs/community/discussions/45969))
- **Content-type mismatch on the index itself** (OCI `image.index.v1+json` vs Docker `manifest.list.v2+json`) has broken strict consumers (Dependabot) — a reminder that OCX's own OCI client should accept both media types for an index, not hard-match one.
- No GHCR-specific deviation found in **how** it stores/serves multi-platform indices beyond the two quirks above — the underlying mechanics (one manifest-list/image-index referencing per-platform manifests by digest) are spec-standard.

## 7. Risks for OCX & Mitigation

**What breaks if GHCR throttles/degrades a CI-heavy consumer:**

- A GitHub Actions workflow (or any CI) resolving `ocx.rs/<pkg>` → `ghcr.io/ocx-contrib/<pkg>` at install time depends on GHCR for both (a) the index-resolved location being reachable and (b) the actual blob pull completing in reasonable time. The **regional CDN slowdown pattern** (item 5) means a runner physically in an affected region could see multi-minute-per-layer pulls even though GHCR's status page is green and nothing is "down" — this reads to a user as "OCX is slow/broken," not "GHCR's Fastly edge is degraded."
- The **push-side 429** pattern (item 5) is a risk for `ocx-contrib`'s own mirror-publish CI (the pipeline populating GHCR from `ocx-mirror`), not for downstream consumers — concurrent mirror jobs pushing many packages/tags could see failed publishes with no built-in retry.
- **No SLA** means OCX cannot contractually promise availability better than "whatever GHCR has today," which is fine for a backend tool per `product-context.md` positioning but should be stated, not assumed.

**Mitigation — precedent already exists in-tree.** OCX already ships (per [`adr_oci_registry_mirror.md`](./adr_oci_registry_mirror.md)) a client-declared `[mirrors."<host>"]` config that rewrites host + repo-path-prefix with replace semantics and mandatory digest verification, originally designed for blocked-egress corporate networks fronting GHCR/Docker Hub with Artifactory/Nexus/Harbor. **The exact same mechanism is the answer to GHCR throttling a public-catalog consumer**: an org hitting persistent GHCR degradation can point `[mirrors."ghcr.io"]` at their own pull-through cache (or at a CDN-fronted mirror OCX itself might operate later) with zero change to the canonical `ocx.rs/<pkg>` identifier or lockfile portability (D11's storage-keying invariant already guarantees this). No new engineering — this is a "the mitigation already shipped for a different reason" situation, which is the best kind.

For the `ocx-contrib` publish side specifically (not yet covered by the existing mirror ADR, which only rewrites the *read* path): mitigate push-429s with bounded retry/backoff in the mirror-publish CI step (not in OCX itself — this is `ocx-mirror`/workflow-level, per the "generic utility vs command-specific glue" placement rule in `quality-core.md`) and avoid unbounded push concurrency in the mirror matrix.

---

## Risk Table

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| 1 | GHCR introduces public-pull rate limits with no/short notice | Low–Medium (no numeric limit today, but explicitly "subject to change") | High — breaks the "no rate limit" assumption the ADR leans on | `[mirrors]` config (already shipped) lets any consumer redirect reads to a self-hosted or third-party mirror without identifier/lockfile change |
| 2 | Regional CDN (Fastly) degradation slows pulls for a subset of users/regions | **Confirmed recurring**, 9+ months of reports, daily pattern in EU | Medium — perceived as "OCX is slow," support burden | Document as a known GHCR characteristic in troubleshooting docs; `[mirrors]` is the user-side fix; consider a status-check note in install-failure diagnostics |
| 3 | Push-side `429` under concurrent mirror-publish CI | Medium (reported under concurrent `docker/build-push-action` pushes) | Low–Medium — publish pipeline flakiness, not user-facing | Bounded retry/backoff + concurrency cap in `ocx-mirror` publish workflow (not OCX core) |
| 4 | No Referrers API — can't attach/discover provenance/SBOM via standard OCI mechanism | Certain (confirmed gap) | Low today (OCX has no provenance feature yet); Medium if supply-chain-signing ADR (already flagged as Deferred in the index ADR) lands | Use GitHub Attestations API directly (bypasses referrers) or defer to when/if GHCR ships 1.1 support; don't design the signing ADR around referrers-on-GHCR |
| 5 | No native tag immutability / retention GC | Certain (confirmed gap, open feature request) | Low for OCX (cascade-tag model already treats tags as mutable pointers per subsystem-oci.md "OCI tags mutable" gotcha) | None needed — already the assumed model; digest is the only immutable anchor, which is already OCX's invariant |
| 6 | `_catalog` unusable for discovery | Certain (confirmed gap) | None — already routed around by D1/D2 (index-indirection, resolution vs. discovery split) | Already mitigated by design; this research **validates** the ADR's stated rationale rather than surfacing new work |
| 7 | Fine-grained PATs unsupported for Packages | Certain (confirmed gap) | Low–Medium — org security policies phasing out classic PATs will friction on any human-issued push credential | Prefer `GITHUB_TOKEN` (repo-scoped, works today, no fine-grained-PAT dependency) for CI publishing per D4/D5; document classic-PAT-only as a known constraint if a human-issued announce credential is ever needed |
| 8 | Multi-arch image index polluted with `unknown/unknown` platform entries from buildx attestations | Medium (depends on whether mirror/publish pipeline enables provenance/SBOM) | Medium — could corrupt `Platform::select()` if a non-platform manifest is treated as a candidate | Ensure `ocx-mirror`/publish tooling disables buildx `provenance`/`sbom` flags, or ensure `fetch_candidates()` tolerates/filters entries with no valid `Platform` annotation |
| 9 | Package-repo link required before `GITHUB_TOKEN` push works reliably | Medium (footgun on first push per new mirror repo) | Medium — silent push failures in a fresh `ocx-contrib` repo | Explicitly link package↔repo (or add `org.opencontainers.image.source` label) on first publish rather than relying on auto-link |

## Recommendation

Proceed with GHCR (`ghcr.io/ocx-contrib/*`) as physical storage exactly as the existing ADR already decided — nothing in this research argues against it, and item-by-item it confirms the ADR's own reasoning (`_catalog` dead-end, D1; content-addressed digest as the only trustworthy anchor, D3). The one concrete addition this research motivates: **treat the already-shipped `[mirrors]` config as GHCR's designated relief valve, and say so explicitly in user-facing docs** (troubleshooting / corporate-deployment guide) — "if GHCR pulls are slow in your region or you hit a future rate limit, point `[mirrors."ghcr.io"]` at a pull-through cache" is a one-line doc addition that turns a confirmed, recurring, externally-caused reliability issue into a documented, self-service fix instead of a support ticket. On the publish side, add bounded retry to the `ocx-mirror`/`ocx-contrib` CI push step and make repo↔package linking explicit on first publish — both small, `ocx-mirror`-repo-scoped changes, not OCX core work. Do not build anything around GHCR's Referrers API; if/when the deferred signing/provenance ADR is drafted, assume GHCR support is absent and design against GitHub's Attestations API or a registry-agnostic mechanism instead.

## Sources

- [GitHub Docs — Working with the Container registry](https://docs.github.com/en/packages/working-with-a-github-packages-registry/working-with-the-container-registry) — visibility defaults, layer/upload limits, scopes
- [GitHub Docs — Introduction to GitHub Packages](https://docs.github.com/en/packages/learn-github-packages/introduction-to-github-packages) — free-for-public billing, `GITHUB_TOKEN` vs PAT
- [GitHub Docs — REST API endpoints for packages](https://docs.github.com/en/rest/packages/packages?apiVersion=2022-11-28) — delete permissions, 30-day restore, 5,000-download deletion block
- [GitHub Docs — Permissions required for fine-grained PATs](https://docs.github.com/en/rest/authentication/permissions-required-for-fine-grained-personal-access-tokens) — Packages excluded
- [toddysm.com — Authenticating with OCI Registries: GHCR Implementation](https://toddysm.com/2024/02/12/authenticating-with-oci-registries-github-container-registry-ghcr-implementation/) — concrete anonymous/bearer token flow
- [github/community#49671 — Possible rate limits for pulling images from ghcr.io?](https://github.com/orgs/community/discussions/49671)
- [github/community#163029 — Does GHCR support the Referrers API?](https://github.com/orgs/community/discussions/163029)
- [github/community#26267 — Delete image tag from GHCR](https://github.com/orgs/community/discussions/26267)
- [github/community#181783 — Immutable image tags feature request](https://github.com/orgs/community/discussions/181783)
- [github/community#25658 — tags/list pagination Link header bug (fixed 2021)](https://github.com/orgs/community/discussions/25658)
- [github/community#26279 — How to check if an image exists on GHCR](https://github.com/orgs/community/discussions/26279)
- [github/community#26299 — Ghcr.io Docker HTTP API (`_catalog` backlog)](https://github.com/orgs/community/discussions/26299)
- [github/community#173607 — Degraded performance with ghcr.io (Fastly root cause)](https://github.com/orgs/community/discussions/173607)
- [github/community#45969 — `unknown/unknown` architecture from attestations](https://github.com/orgs/community/discussions/45969)
- [containerd#4520 — GHCR manifest-list content-type bug (historical)](https://github.com/containerd/containerd/issues/4520)
- [dependabot-core#13383 — OCI image-index vs Docker manifest-list content-type mismatch](https://github.com/dependabot/dependabot-core/issues/13383)
- [docker/build-push-action#1194 — Push does not retry after 429](https://github.com/docker/build-push-action/issues/1194)
- [Docker Docs — Docker Hub pull usage and limits](https://docs.docker.com/docker-hub/usage/pulls/) — current numeric comparison baseline
- [Gecko Security — GHCR Complete Guide (2026)](https://www.gecko.security/blog/ghcr-github-container-registry-guide) — billing/no-rate-limit secondary confirmation
