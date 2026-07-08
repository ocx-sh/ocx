# Official Docker Images for ocx (issue #190)

## Context

#190 asks for Docker container deployment. Settled in discussion: publish `ghcr.io/ocx-sh/ocx` (GHCR only, no Docker Hub — revisit via Docker-Sponsored OSS later), two variants (`trixie` default + `alpine`; rpm dropped — zero industry precedent), python-style rolling tag matrix with unsuffixed tags aliasing trixie, amd64+arm64, binaries reused from cargo-dist release archives (never rebuilt), weekly scheduled rebuild of the latest release for base-image CVE freshness, immutable date-stamped tags so old digests never become GC-able. Bootstrap-env gap split off as #193. Docs: full guide `docker.md` + short mention in `installation.md`.

Key mechanism facts (verified):
- `release: published` trigger dead — cargo-dist creates the release with `GITHUB_TOKEN`; token-created events don't fire workflows. Must wire via cargo-dist post-announce.
- Caller job caps called-workflow permissions; dist ≥0.18 supports `github-custom-job-permissions` (applies to post-announce jobs; key = job name without `./`; override REPLACES defaults, list every scope).
- Cron path has no workflow-artifact store → use `gh release download` uniformly in all trigger paths (assets guaranteed present post-announce).
- No QEMU needed: Dockerfiles are FROM/ARG/COPY/ENV/CMD only (TLS roots compiled in via `webpki-root-certs`, Cargo.toml:78).
- `ocx version` subcommand exists (`crates/ocx_cli/src/command/version.rs`) — used for smoke tests.
- Current workspace version 0.4.2, license Apache-2.0.

## Files to create

### 1. `docker/trixie.Dockerfile` + `docker/alpine.Dockerfile`

```dockerfile
# Official ocx image — Debian trixie (glibc) variant, the default.
# Binary extracted from cargo-dist release archive by
# .github/workflows/docker-publish.yml — never compiled here.
FROM debian:trixie-slim@sha256:<RESOLVE-AT-IMPLEMENTATION>
ARG TARGETARCH
COPY --chmod=755 binaries/${TARGETARCH}/ocx /usr/local/bin/ocx
ENV OCX_NO_UPDATE_CHECK=1
CMD ["ocx"]
```

alpine.Dockerfile identical with `FROM alpine:3@sha256:<...>` (workflow feeds musl binaries). Resolve digests via `docker buildx imagetools inspect` — use the manifest-LIST digest. No RUN/USER/ENTRYPOINT. `*.Dockerfile` naming matches Renovate's default dockerfile manager pattern.

### 2. `.github/workflows/docker-publish.yml` — one workflow, three triggers

- `on:` `workflow_call` (input `plan`, dist contract, unused) + `schedule: cron "0 7 * * 1"` + `workflow_dispatch` (optional `tag` input, empty = latest release).
- Workflow-level `permissions: contents: read`; concurrency group `${{ github.workflow }}-${{ github.ref }}` with `cancel-in-progress: false` (never cancel half-pushed multi-tag publish — deliberate deviation from standard snippet, comment why).
- **Job `resolve`**: derive tag by event (`schedule`/`workflow_dispatch` → `gh api repos/$REPO/releases/latest`; else `github.ref_name` — inside a called workflow this is the caller's tag ref). `version=${tag#v}`; `skip=true` if version contains `-` (prerelease); freeze `date=$(date -u +%Y%m%d)` once so both matrix legs share the stamped tag.
- **Job `build`**: `if skip != 'true'`, `needs: resolve`, matrix `include: [{variant: trixie, libc: gnu}, {variant: alpine, libc: musl}]`, `fail-fast: true`. Job permissions: `contents: read, packages: write, id-token: write, attestations: write`. Steps:
  1. checkout (`persist-credentials: false`, repo-standard pin `de0fac2e… # v6.0.2`)
  2. `gh release download "v$VERSION" --pattern "ocx-*-unknown-linux-${LIBC}.tar.gz" --dir archives`
  3. Stage: extract both arches → `build-context/binaries/{amd64,arm64}/ocx` (map x86_64→amd64, aarch64→arm64); fail hard if binary missing; smoke `./build-context/binaries/amd64/ocx version | grep -F "$VERSION"`
  4. Compute tags in bash (metadata-action templating buys nothing since version pre-resolved): variant leg gets `V-<variant>`, `X.Y-<variant>`, `X-<variant>`, `<variant>`, stamped `V-<variant>-<DATE>`; trixie leg additionally `V`, `X.Y`, `X`, `latest` (14 tags total at current versioning)
  5. `docker/setup-buildx-action@bb05f3f5519dd87d3ba754cc423b652a5edd6d2c # v4.2.0`
  6. `docker/login-action@af1e73f918a031802d376d3c8bbc3fe56130a9b0 # v4.4.0` (ghcr.io, `github.actor` / `github.token`)
  7. `docker/metadata-action@dc802804100637a589fabce1cb79ff13a1411302 # v6.2.0` — labels/annotations only (`DOCKER_METADATA_ANNOTATIONS_LEVELS: manifest,index`); explicit labels: vendor=ocx.sh, url=https://ocx.sh, documentation=https://ocx.sh/docs/docker, licenses=Apache-2.0, version=$VERSION (cron has no tag ref; auto: source/revision/created)
  8. `docker/build-push-action@53b7df96c91f9c12dcc8a07bcb9ccacbed38856a # v7.3.0` — `context: build-context`, `file: docker/${{ matrix.variant }}.Dockerfile`, `platforms: linux/amd64,linux/arm64`, `push: true`, `provenance: mode=max` (captures base.name/base.digest), `sbom: true`
  9. `actions/attest-build-provenance` (reuse pin from build-windows-shims.yml), `subject-digest` from push output, `push-to-registry: true`
  10. Smoke: `docker run --rm ghcr.io/…:<stamped-tag> ocx version | grep -F "$VERSION"`
- Secrets discipline: only `github.token`, always via `env:`; all actions SHA-pinned per subsystem-ci.md; no QEMU step (add back only if a `RUN` ever lands in a Dockerfile).

### 3. `website/src/docs/docker.md`

Per docs-style.md (narrative intros, `{#anchor}` every heading, reference-style links at bottom):
1. `# Docker {#docker}` — official images = release binary on slim base, `ghcr.io/ocx-sh/ocx`
2. `## Images and tags {#docker-tags}` — variant table, full tag matrix, arch, rebuild policy: rolling tags move weekly (base refresh), date-stamped tags immutable → pin those (or digests) for reproducibility
3. `## Copying the binary into your image {#docker-copy-from}` — `COPY --from=ghcr.io/ocx-sh/ocx:X.Y.Z-alpine /usr/local/bin/ocx /usr/local/bin/ocx`; musl static copies anywhere, gnu needs glibc base
4. `## Baking a project toolchain {#docker-project-toolchain}` — `COPY ocx.toml ocx.lock ./` → `RUN ocx pull` → `ENTRYPOINT ["ocx", "--offline", "run", "--"]`; why not runtime lazy-pull (registry availability/creds/cold-start; `--offline` turns drift into hard error)
5. `## Private registries at build time {#docker-build-auth}` — BuildKit secrets: `RUN --mount=type=secret,id=ocx_token,env=OCX_AUTH_OCX_SH_TOKEN ocx pull`; never ENV; cross-link `reference/environment.md` `OCX_AUTH_<REGISTRY>_*`
6. `## Bootstrapping single tools {#docker-mini-project}` — mini-project pattern (two-line ocx.toml + lock); note richer bootstrap story tracked separately (#193, don't link issue in docs — phrase as "planned")
7. Cross-link `in-depth/ci.md`

## Files to modify

### 4. `dist-workspace.toml` + regenerated `release.yml`

```toml
post-announce-jobs = ["./post-release-oci-publish", "./deploy-website-release", "./docker-publish"]
github-custom-job-permissions = { docker-publish = { contents = "read", packages = "write", id-token = "write", attestations = "write" } }
```

Then `dist generate-ci`; commit both together (`verify-release-ci.yml` polices drift). Verify diff: new `custom-docker-publish` job carries explicit `permissions:` block.

### 5. `renovate.json` — packageRule

```json
{
  "description": "Dockerfile base images — group, keep digests pinned, ci(deps) prefix",
  "matchManagers": ["dockerfile"],
  "groupName": "docker-base-images",
  "semanticCommitType": "ci",
  "semanticCommitScope": "deps",
  "pinDigests": true
}
```

### 6. `website/.vitepress/config.mts` — sidebar entry `{ text: "Docker", link: "/docs/docker" }` after Installation (~line 54)

### 7. `website/src/docs/installation.md` — new `## Docker {#docker}` section (between Manual Installation and Updating): 2–3 sentences + `docker run --rm ghcr.io/ocx-sh/ocx ocx version`, reference-style link to docker.md

## Documentation surfaces (complete list)

`website/src/docs/docker.md` (new), `website/src/docs/installation.md`, `website/.vitepress/config.mts`. No env-var changes → `reference/environment.md` untouched. `in-depth/ci.md` gets nothing (docker.md links to it, one-way).

## Step ordering

1. Branch (worktree branch, not main). Copy this plan to `.claude/artifacts/plan_docker_images.md` (project convention).
2. Resolve base digests; write both Dockerfiles.
3. Write `docker-publish.yml`; `task ci:actionlint`.
4. Edit `dist-workspace.toml`; `dist generate-ci`; inspect release.yml diff.
5. Renovate rule.
6. Docs (docker.md, sidebar, installation.md); website build/link check.
7. Local build verification (below); commit(s) per workflow-git; reference #190.

## Verification

Pre-merge (local):
- `gh release download v0.4.2 --pattern 'ocx-*-unknown-linux-gnu.tar.gz'`, replicate staging, `docker buildx build --platform linux/amd64,linux/arm64 -f docker/trixie.Dockerfile <ctx>` (no push) + `--load` amd64 run `ocx version`. Repeat alpine/musl.
- `task ci:actionlint`; `dist generate --check`; website build for links/anchors.

Post-merge:
- Backfill: `gh workflow run docker-publish.yml -f tag=v0.4.2`. Then: both legs green; `docker buildx imagetools inspect ghcr.io/ocx-sh/ocx:latest` shows 2-platform index; 14 tags present; `gh attestation verify oci://ghcr.io/ocx-sh/ocx:latest --owner ocx-sh`; `docker run --rm ghcr.io/ocx-sh/ocx:alpine ocx version`.
- One-time manual: set ghcr package public + confirm repo linkage (first push creates it private).
- Next release: `custom-docker-publish` runs in post-announce phase.
- First Monday: scheduled rebuild moves rolling tags; prior stamped tag still resolves.

## Risks

- ghcr `ocx` package name collision under ocx-sh — check org packages before merge.
- fail-fast partial publish (alpine fails after trixie pushed) — rerun idempotent, acceptable.
- Scheduled workflows auto-disable after 60d repo inactivity — noted, unlikely.
- glibc floor: cargo-dist gnu builds (ubuntu-22.04, glibc 2.35) ≤ trixie 2.41 today; smoke test catches future drift.
- `alpine:3` → future `alpine:4` needs manual FROM bump (intentional).
