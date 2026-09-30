# Research: Registry delete behaviour

## Metadata

**Date:** 2026-09-29
**Domain:** oci-registry
**Triggered by:** adr snapshot lifecycle
**Expires:** 2027-03-29

## Direct Answer

DELETE support in OCI registries is **optional by spec** and inconsistent in practice. An
`ocx` snapshot-prune command cannot rely on `DELETE /v2/<name>/manifests/<tag>` (or by
digest) working uniformly — GHCR and Docker Hub reject it outright over the distribution
API, some registries 405/400, and deletion never cascades to per-platform manifests, OCI
referrers, or cosign's legacy `.sig` tags. A robust implementation needs registry-shape
detection, an explicit referrer/keep-tag/signature sweep before the subject delete, and a
documented "prune reduced storage on registries with online GC; on others it only unlists
until an operator-run GC" caveat.

## 1. OCI distribution-spec DELETE semantics

- Two endpoints: `DELETE /v2/<name>/manifests/<tag>` (tag delete, spec-introduced in 1.1)
  and `DELETE /v2/<name>/manifests/<digest>` (digest delete — removes the manifest and
  **every** tag pointing at it, since digest delete).
- Response codes: **202** success, **404** repo/manifest missing, **400**/**405** if the
  registry disables deletion. Spec text: "Registries MAY implement deletion or they MAY
  disable it" — opt-in per operator, not a client guarantee.
  [opencontainers/distribution-spec](https://github.com/opencontainers/distribution-spec/blob/main/spec.md),
  [conformance #3234](https://github.com/docker/distribution/issues/3234).
- No cascade language: deleting an image index does not remove its per-platform child manifests — they become unreferenced, not deleted.
- `distribution/distribution` (`registry:2`) needs `REGISTRY_STORAGE_DELETE_ENABLED=true`
  (default off). Delete only marks content unreferenced; bytes reclaim via a separate
  `registry garbage-collect` (registry should be read-only/stopped during the run so an
  in-flight push isn't swept). [CNCF GC docs](https://distribution.github.io/distribution/about/garbage-collection/).

## 2. Deleting an index; multi-tag digests; referrers on delete

- **Index children are never auto-removed.** A digest delete of the index manifest
  removes the index object and all tags pointing at it; the per-platform manifests it
  referenced become unreferenced (orphaned) but stay on disk until GC sweeps them.
- **Digest delete removes every tag on that digest**, not just the one you asked about —
  the operational trap for a "rolling" tag (e.g. `latest`) that shares a digest with the
  newest pinned tag: deleting the pinned tag by digest silently deletes `latest` too.
  `skopeo delete` resolves a tag argument to its digest and deletes by digest, so it has
  the same blast radius even when invoked with a tag name.
  [flightaware.engineering](https://flightaware.engineering/how-hard-is-it-to-delete-a-docker-tag/).
- **Fallback when tag DELETE 400/405s**: push a small dummy manifest to the target tag,
  fetch its digest, then `DELETE` the manifest by that digest. Widely used (`regctl tag
  rm`, several bespoke cleanup scripts) because digest-delete support is more consistent
  than tag-delete support. Risk: if the registry *also* disables digest delete, the dummy
  manifest is left behind as garbage; and the overwrite step is itself a write against a
  potentially-shared tag — a second writer racing the same tag between overwrite and
  delete sees a manifest that never round-trips back.
  [regclient/regclient#335](https://github.com/regclient/regclient/issues/335),
  [regctl tag delete docs](https://regclient.org/cli/regctl/tag/delete/).
- **Referrers**: the distribution-spec referrers API has no delete-cascade either —
  deleting a subject manifest does not delete its referrers. For registries answering the
  referrers API natively (1.1+, OCI-native GC-aware stores), an orphaned referrer is
  reachable only by repository listing, not by a `subject` back-pointer once the subject
  is gone. For the **tag-schema fallback** (`sha256-<hex>` index tag, used when
  `/v2/<name>/referrers/<digest>` 404s), the client itself owns the update: on delete of
  the subject it "MAY" pull the fallback index, remove the descriptor for the deleted
  manifest, and push the trimmed index back, or delete the fallback tag entirely if it
  becomes empty — the spec phrases this as a MAY, not a MUST, so a naive client leaves it
  stale. Cosign's legacy signature scheme (`<repo>:sha256-<hex>.sig` plus separate
  `.att`/`.sbom` tags, still supported in Cosign v3 alongside the newer bundle format) is
  the same shape: it is **not** a referrer the registry tracks, so nothing removes it
  automatically — a client-side `cosign clean`/explicit delete of the `.sig`/`.att`/`.sbom`
  tags is the only way to drop them. [cosign SIGNATURE_SPEC](https://github.com/sigstore/cosign/blob/main/specs/SIGNATURE_SPEC.md).

## 3. Storage reclaim (GC) per registry

- **distribution/distribution**: manual, offline-preferred `registry garbage-collect`;
  mark-and-sweep over the whole blob store.
- **GitLab**: cleanup policy deletes *tags* only (regex, `keep_n`, `older_than`); a
  separate online GC worker (on by default on GitLab.com, opt-in self-managed via
  `gitlab-ctl registry-garbage-collect`) reclaims bytes. Tag delete moved off a
  GitLab-proprietary endpoint (`DELETE /v2/<name>/tags/reference/<tag>`, deprecated 16.4,
  removal targeted 17.0) onto the spec's `manifests/<tag>`.
  [API docs](https://docs.gitlab.com/api/container_registry/),
  [deprecation](https://gitlab.com/gitlab-org/container-registry/-/issues/1094).
- **Harbor**: tag retention policies (keep-N / keep-newer / pattern) run as scheduled
  jobs; a separate GC job (registry read-only during the run) reclaims blobs.
  [GC docs](https://goharbor.io/docs/2.0.0/administration/garbage-collection/).
- **zot**: standard DELETE; GC is **inline**, no read-only window, config-driven (`gc`,
  `gcInterval`, `gcDelay`); removes untagged manifests by default unless still referenced.
- **GHCR**: distribution DELETE is unusable for real cleanup — the real path is the
  separate GitHub Packages REST API (`DELETE .../versions/{version_id}`), keyed by
  **version ID** not tag/digest (list first to resolve), PAT needs
  `packages:read`+`delete:packages`. No GC step; delete is immediate.
- **Docker Hub**: registry-API manifest DELETE returns `UNSUPPORTED`. Deletion is Hub's
  own REST API (`DELETE /v2/repositories/<ns>/<repo>/tags/<tag>/`), rate-limited (100
  req/10 min free tier). No client-triggered GC.
- **Artifactory**: standard DELETE plus AQL bulk delete and a
  [multi-arch-aware delete flow](https://jfrog.com/help/r/jfrog-artifactory-documentation/delete-multi-architecture-docker-tags)
  that cleans an index and its children together. GC internal/scheduled.
- **ECR**: no distribution DELETE; `BatchDeleteImage` only (`imageTag` or `imageDigest`
  in `imageIds`) — last-tag-removed deletes the image; digest delete removes image + all
  tags. Lifecycle policies run server-side. Reclaim is immediate.
- **ACR / GAR**: same shape as ECR — provider APIs (`az acr repository delete|untag`,
  `gcloud artifacts docker images delete`), not distribution DELETE; immediate reclaim.

## 4. Race and idempotency

- **Idempotency**: a second DELETE of an already-gone tag/digest returns 404, not a
  repeated 202 — treat 404 as success for at-least-once retry loops.
- **TOCTOU between list and delete**: `list_tags` → decide-what's-stale → `DELETE` isn't
  atomic against a concurrent push. A window exists where a tag ocx marked prunable gets
  repointed (rolling alias reuse, concurrent `announce`) before the delete lands.
  Digest-based delete narrows but doesn't close this — the digest can gain a *new* tag in
  that window, and a digest delete nukes it along with the intended one.
- **Multi-tag digest sharing** (§2) is the sharpest safety issue for automatic prune:
  before deleting by digest, re-list tags-by-digest (not universally exposed) or restrict
  the automatic path to tag-delete only, never digest-delete.

## 5. How existing tools implement prune

| Tool | Mechanism | Fallback when tag-DELETE unsupported |
|---|---|---|
| `regctl tag rm` / `regctl image delete` | Tag DELETE, else digest DELETE | Push unique dummy manifest to the tag, delete by its digest ([docs](https://regclient.org/cli/regctl/tag/delete/)) |
| `crane delete` | Raw DELETE of whatever ref (tag or digest) is passed | None — caller's responsibility to resolve |
| `oras manifest delete` | Digest-only DELETE | None; expects a digest, refuses ambiguity |
| `skopeo delete` | Resolves tag → digest, deletes by digest | N/A (always digest-path; explicitly deletes all co-tags) |
| `snok/container-retention-policy` | GHCR Packages API by version ID | Resolves each kept tag's index, excludes child digests from delete candidates |
| GitLab cleanup policy | Server-side scheduled job against its own registry, not a generic client | Separate online/offline GC reclaims bytes after |

## 6. `ocx_oci` client shape (repo check)

`crates/ocx_oci/src/client.rs` and `crates/ocx_oci/src/client/transport.rs`
(`OciTransport` trait) expose push/pull/mount/list/catalog/referrers methods only — no
`delete_manifest`/`delete_tag` anywhere. The vendored fork at
`external/rust-oci-client/src/client.rs` is the same: `list_tags`, `pull*`, `push*`,
`mount_blob`, `pull_referrers_native`, `catalog` — no `Method::DELETE` call exists in
either crate today (`grep -rn "DELETE" crates/ocx_oci/src external/rust-oci-client/src`
returns zero matches). Adding delete means a new `OciTransport::delete_manifest(&self,
reference) -> Result<()>` variant implemented on `native_transport.rs` and
`test_transport.rs` — the transport is already method-abstracted (HTTP verb + path is
built per call, not hardcoded), so wiring the raw call through is mechanical; the design
work is entirely in the policy layer above it (§2–§4).

## Recommendation

1. **DELETE is best-effort, never load-bearing for reclaim.** ocx's prune command is
   "unlist + best-effort cleanup", not "guaranteed reclaim" — Docker Hub and GHCR don't
   support the distribution DELETE at all.
2. **Order of operations per pruned snapshot**, all tolerant of 404/already-gone:
   a. delete the `__ocx.keep.<algo>-<hex>` platform-manifest tags first, by tag — that's
      exactly the "let this get orphan-swept" signal;
   b. delete cosign `.sig`/`.att`/`.sbom` legacy tags and referrers-fallback-index entries
      for the manifests being pruned *before* the subject — orphaning a signature after
      its subject is gone leaves a verifier a dangling descriptor;
   c. delete the index/manifest tag via `DELETE /v2/<name>/manifests/<tag>`;
   d. **never** delete by digest in the automatic path. Sacrifices cleanup on registries
      that only support digest-delete-with-dummy-overwrite, but avoids the
      multi-tag-shares-digest foot-gun (§2, §4) entirely; gate the overwrite+digest-delete
      fallback behind an explicit, non-default flag with a loud warning.
3. **Detect registry capability once per host, cache it** — extend
   `host_capabilities.rs` (already does referrers-API discovery) with a
   `supports_manifest_delete` probe or a seeded allow/deny-list from the table below.
4. **404 on delete is success**, not an error — trivializes retry/partial-failure resume.
5. **Document it**: prune "removes tags"; bytes reclaim via the registry's own GC
   (immediate on ECR/ACR/GAR/GHCR/Hub, scheduled/manual on distribution/GitLab/Harbor,
   inline on zot) — ocx has no cross-registry lever to force reclaim.

### Per-registry support snapshot

| Registry | Distribution-spec tag DELETE | Digest DELETE | Native delete path | GC after delete |
|---|---|---|---|---|
| distribution/distribution (`registry:2`) | Yes, if `REGISTRY_STORAGE_DELETE_ENABLED=true` | Yes (same flag) | — | Manual, offline-preferred |
| GitLab Container Registry | Yes (`manifests/<tag>`, proprietary endpoint deprecated) | Yes | Cleanup policy (tags) | Online (gitlab.com) / opt-in self-managed |
| Harbor | Yes | Yes | Retention policy + API | Scheduled, read-only window |
| zot | Yes | Yes | — | Inline, no downtime |
| GHCR | No (distribution API) | No | GitHub Packages API by version ID | Immediate |
| Docker Hub | No (`UNSUPPORTED`) | No | Hub REST API by tag | Immediate |
| Artifactory | Yes | Yes | + AQL bulk delete, multi-arch-aware | Immediate/scheduled |
| ECR | No | No | `BatchDeleteImage` (tag or digest) | Immediate |
| ACR | No | No | `az acr repository delete/untag` | Immediate |
| GAR | No | No | `gcloud artifacts docker images delete` | Immediate |

## Sources

| Source | Type | Date | Relevance |
|---|---|---|---|
| [opencontainers/distribution-spec spec.md](https://github.com/opencontainers/distribution-spec/blob/main/spec.md) | Spec | live | DELETE endpoints, status codes, optionality |
| [docker/distribution#3234](https://github.com/docker/distribution/issues/3234) | Issue | — | 202/400/405 conformance |
| [distribution.github.io GC docs](https://distribution.github.io/distribution/about/garbage-collection/) | Docs | live | `REGISTRY_STORAGE_DELETE_ENABLED`, mark-sweep |
| [GitLab container registry API](https://docs.gitlab.com/api/container_registry/) | Docs | live | Tag delete endpoint migration |
| [gitlab-org/container-registry#1094](https://gitlab.com/gitlab-org/container-registry/-/issues/1094) | Issue | — | Proprietary endpoint deprecation |
| [Harbor GC docs (2.0)](https://goharbor.io/docs/2.0.0/administration/garbage-collection/) | Docs | — | Retention vs GC split |
| [zot whats-new](https://zotregistry.dev/v2.1.11/general/whats-new/) | Docs | live | Inline GC, untagged-manifest defaults |
| [GitHub Packages REST API](https://docs.github.com/en/rest/packages/packages) | Docs | live | GHCR delete-by-version-id, scopes |
| [docker/hub-feedback#1759](https://github.com/docker/hub-feedback/issues/1759) | Issue | — | Docker Hub `UNSUPPORTED` on manifest DELETE |
| [JFrog multi-arch tag delete](https://jfrog.com/help/r/jfrog-artifactory-documentation/delete-multi-architecture-docker-tags) | Docs | live | Index-aware delete |
| [AWS ECR batch-delete-image](https://docs.aws.amazon.com/cli/latest/reference/ecr/batch-delete-image.html) | Docs | live | tag vs digest delete semantics |
| [regclient/regclient#335](https://github.com/regclient/regclient/issues/335) | Issue | — | dummy-manifest fallback risk |
| [regctl tag delete](https://regclient.org/cli/regctl/tag/delete/) | Docs | live | tag-delete-with-fallback mechanism |
| [How Hard is it to Delete a Docker Tag?](https://flightaware.engineering/how-hard-is-it-to-delete-a-docker-tag/) | Blog | — | skopeo digest-resolve, co-tag deletion |
| [cosign SIGNATURE_SPEC.md](https://github.com/sigstore/cosign/blob/main/specs/SIGNATURE_SPEC.md) | Spec | live | `.sig`/`.att`/`.sbom` tag schema, v3 legacy support |
| [snok/container-retention-policy README](https://github.com/snok/container-retention-policy) | Docs | live | multi-arch child protection strategy |
