# Review R1 — SOTA gap check: `ocx package copy`

Persisted by the orchestrator: `worker-researcher` has no Write tool in this
repo, so the worker returned inline and this file is the orchestrator's verbatim
capture. (Known limitation, recorded in `.agents/memory/hex.md`.)

Scope: `crates/ocx_lib/src/oci/copy.rs`, `crates/ocx_lib/src/publisher/copy.rs`
compared against crane, skopeo, oras, regctl, cosign, `docker buildx imagetools create`.

## Direct answer

OCX's design is more sophisticated than any single reference tool on referrer
recursion and transactional write ordering, deviates deliberately (and
defensibly) on index-merge semantics, and has one concrete, code-verified
functional gap in the cross-repo blob-mount auth path.

## Gaps

### [High] Cross-repo blob mount likely never succeeds on strict-scope registries

`copy_blob` (`oci/copy.rs:253-256`) calls only `ensure_auth(&target_image, Push)`
before `mount_blob`. The fork (`external/rust-oci-client/src/client.rs:1882-1900`)
authenticates via `apply_auth(image, RegistryOperation::Push)` against the target
only; the scope string built at `client.rs:930-933` is
`repository:{target}:pull,push` — no scope is ever requested for the **source**
repository. A mount (`POST .../blobs/uploads/?mount=<digest>&from=<source>`) needs
pull on `from` *and* push on the target.

Fallback is graceful (`native_transport.rs:490-499` maps any mount error, auth
included, to `MountOutcome::UploadRequired`), so this is not a correctness bug —
but it likely makes the mount optimization dead code on GHCR/ACR/Harbor/GitLab,
defeating the stated design goal in the module's own doc comment.

Fix direction: request a combined scope
(`scope=repository:target:push&scope=repository:source:pull` in one token call,
RFC-legal per the distribution auth spec), or call `ensure_auth(&source_image, Pull)`
before attempting the mount. Fork-level capability gap as well as an OCX-side one —
worth an issue against the fork per the repo convention ("fork — …").

- https://github.com/docker/distribution/issues/634
- https://gitlab.com/gitlab-org/gitlab-foss/-/issues/40197
- https://github.com/moby/moby/issues/38221

### [Warn] Index-merge-per-platform deviates from every field tool's default

- `crane copy` replaces the index wholesale — no per-platform merge exists in
  `pkg/crane/copy.go` (fetch descriptor, push descriptor, nothing else).
- `skopeo copy --all` / `--platform-list` copies the whole index or produces a
  **sparse** index most registries reject.
- `oras cp`: "When copying an image index, all of its manifests will be copied."
- `docker buildx imagetools create` with one index source "performs a carbon copy";
  the only merge-adjacent behaviour is the **opt-in** `--append`.

OCX's default (`Disposition::{Added,Unchanged,Replaced,KeptNotInSource}`,
`publisher/copy.rs:186-220`) silently preserves target platforms the source lacks —
the opposite of what a `crane copy`-literate user expects. Well-reasoned ADR
decision, not an oversight, but it is the single point where OCX most surprises
users from the reference tools. Recommend `--help`/docs state plainly: "unlike
crane/skopeo, a copy merges into the target's existing index rather than replacing it."

### [Warn] No referrers fallback tag scheme; hard refusal (exit 84)

The distribution spec's client-conformance language is normative: a client pushing
a manifest with a `subject` field **MUST** verify the referrers API is available
**or fall back** to the referrers tag schema. regclient implements the fallback
(`--digest-tags`, `sha256-<hex>`); oras has a preview
`--from-distribution-spec=v1.1-referrers-tag` path. OCX's
`ensure_target_serves_referrers` (`oci/copy.rs:196-215`) refuses outright.

Given the 2024–2025 native-support wave (Harbor, Quay, ACR, GHCR, ECR, GitLab),
refusing is a reasonable MVP simplification — but it is a real gap against
self-hosted/older registries (plain `registry:2`, older Artifactory/Nexus, local
dev registries) and a deviation from the spec's own client contract. Track as a
documented limitation rather than a defect.

## Confirmed sound

- **Recursive referrer copy matches oras's design and exceeds crane's.** `crane copy`
  has zero referrer-copying logic; `crane referrers` is listing-only. `oras cp -r`
  does the same recursive climb but is `[Preview]`. Both oras and regctl make it
  **opt-in**; OCX defaults it **on**, which is the more correct choice for a product
  whose selling point is that signatures and lock pins survive promotion.
- **Depth cap (8) + count cap (256) + cycle detection (`seen: BTreeSet`)** has no
  documented analog in oras/regctl/cosign. Ahead of the field, not at parity.
- **Digest-preservation-by-design matches crane's stated intent.** No evidence any
  OCI-conformant registry rewrites manifest bytes server-side on ingest — that would
  fail the distribution-spec conformance suite. The one real digest-drift bug found
  (docker/cli#3394, open since 2021) is **client-side** Docker Engine reformatting and
  does not apply to raw-byte-PUT tools. Caveat (Suggest): stricter schema-validating
  registries could **reject** (not rewrite) a manifest missing fields some registries
  treat as optional (e.g. top-level `mediaType`) — worth a smoke test against the real
  target-registry set.
- **Two-phase write ordering** is more careful about partial failure than any reference
  tool; none document or implement phased copy semantics.
- **Cross-repo mount restricted to same-registry** matches `regctl image copy` exactly
  and is the only sane reading of the spec's same-registry-only mount primitive.
- **Spool + re-hash before push** correctly attributes a mismatch to the source
  registry rather than the destination's rejection. Parity with the field.

## Trends

- **Established**: byte-verbatim manifest copy as the digest-preservation mechanism;
  same-registry-only cross-repo mount; broad native Referrers API support (2024–25).
- **Trending**: recursive referrer copy tooling is still "Preview" even in oras — the
  ecosystem is converged on the spec, not yet on tooling maturity.
- **Declining**: the referrers tag-fallback scheme is increasingly vestigial — the
  strongest argument for OCX's decision not to implement it.
- **Emerging**: no reference tool implements a phased / partial-failure-safe promotion
  model. A genuine differentiator worth stating in the ADR and docs.

## Sources

- https://github.com/google/go-containerregistry/blob/main/pkg/crane/copy.go
- https://github.com/google/go-containerregistry/blob/main/cmd/crane/doc/crane_copy.md
- https://github.com/containers/skopeo/blob/main/docs/skopeo-copy.1.md
- https://oras.land/docs/commands/oras_cp/
- https://regclient.org/cli/regctl/image/copy/
- https://github.com/opencontainers/distribution-spec/blob/main/spec.md
- https://github.com/docker/buildx/blob/master/docs/reference/buildx_imagetools_create.md
- https://github.com/docker/cli/issues/3394

No CVE specific to registry-copy / manifest-confusion / referrer-poisoning /
mount-abuse found in the last ~24 months. Stated explicitly as a gap in evidence
rather than an assertion that no such CVE class exists.

## Recommendation

Ship as-is on index-merge and referrer-recursion semantics. But (1) fix or
file-and-track the cross-repo mount auth-scope gap, since it silently defeats the
feature's stated efficiency goal on the most common registry hosts; and (2) add one
explicit doc/help sentence contrasting merge-not-replace against `crane copy`
semantics. The referrers-fallback gap (exit 84) is fine as a documented limitation,
but should be tracked rather than silently absent from the docs.
