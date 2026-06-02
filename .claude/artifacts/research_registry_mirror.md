# Research: Registry Mirroring & Source Redirection — Prior Art for OCX

**Date:** 2026-06-02
**Domain:** oci, configuration, infrastructure, package-manager
**Triggered by:** ADR for client-declared registry mirror / source replacement (issue #122, part of #111 corporate-infrastructure milestone)
**Consumed by:** [`adr_oci_registry_mirror.md`](./adr_oci_registry_mirror.md)

## Direct Answer

Surveyed registry-mirror / source-redirection mechanisms across containerd, Docker, Cargo, Go modules, Maven, npm, and the three dominant enterprise artifact managers (Artifactory, Nexus, Harbor), plus the two leading binary managers (mise, aqua). Three models are relevant for OCX:

1. **containerd `hosts.toml`** — strongest *structural* fit (per-upstream-host mirror, path preserved, `?ns=` carries upstream context, ordered list + fall back to origin).
2. **Cargo `[source] replace-with`** — strongest *identity/integrity* model (mirror MUST serve bit-identical content; checksum verification mandatory). Cargo deliberately splits `[source] replace-with` (transparent mirror, same identity) from `[registries]` (different identity).
3. **Maven `<mirrorOf>`** — richest *selection* model (`*`, `external:*`, comma lists, `!exclusions`).

Enterprise artifact managers (Artifactory / Nexus / Harbor) **universally insert a repo-key / project-name path prefix** between their host and the upstream path. This is the dominant middleware-OCI-proxy URL convention and the single most important finding for OCX.

## Key Finding — The Repo-Key Path Prefix Is Universal

| Proxy | Client pull URL | Forwarded upstream request |
|-------|-----------------|----------------------------|
| Artifactory (path method) | `company.jfrog.io/<repo-key>/library/nginx:1.25` | `registry-1.docker.io/library/nginx:1.25` |
| Nexus | `nexus.corp/repository/<repo-name>/library/nginx:1.25` | `<remote-storage-url>/library/nginx:1.25` |
| Harbor | `harbor.corp/<proxy-project>/library/nginx:1.25` | `<endpoint>/library/nginx:1.25` |

The upstream image path is forwarded **verbatim**; the proxy's own repo identifier is a **path prefix** the client must include. Artifactory and Nexus also offer subdomain/port methods that avoid the prefix, but those require wildcard DNS/TLS or per-repo ports and are less common. **A mirror feature that only rewrites the host (containerd's default) cannot target an Artifactory path-method remote repo** — host *and* a repository-path prefix must both be rewritable.

## Model-by-Model

### 1. containerd `hosts.toml`
Doc: https://github.com/containerd/containerd/blob/main/docs/hosts.md

- Path: `/etc/containerd/certs.d/<upstream-host>/hosts.toml` (directory name = upstream host).
- `server = "https://registry-1.docker.io"` = canonical upstream / final fallback.
- `[host."https://mirror"]` entries tried in declaration order *before* `server`.
- `capabilities = ["pull", "resolve", "push"]` restricts what each host may serve.
- `override_path = true` lets the mirror embed its own `/v2/` root.
- Upstream context passed as `?ns=<upstream-host>` query param; **image path not rewritten**.
- Integrity: none enforced by containerd — digest pinning in the reference is the only guarantee.
- Matching: exact host (directory name); multiple hosts = ordered fallback for one upstream.

### 2. Docker daemon `registry-mirrors`
Doc: https://docs.docker.com/docker-hub/image-library/mirror/

- `daemon.json` `{"registry-mirrors": ["https://mirror"]}`.
- **docker.io only** — cannot mirror ghcr.io/quay.io/private. Baked into the hub-specific token flow.
- Architecturally unfit for OCX (hub-only, no per-registry scope, no path awareness).

### 3. Cargo source replacement
Docs: https://doc.rust-lang.org/cargo/reference/source-replacement.html · https://doc.rust-lang.org/cargo/reference/config.html#registries

```toml
[source.crates-io]
replace-with = "my-mirror"
[source.my-mirror]
registry = "https://mirror.corp/crates-io-index"
```

- **Hard rule: the replacement must serve bit-identical crates with matching checksums.** Cargo verifies the `.crate` checksum against the index entry; divergence fails the build. Non-bypassable.
- Deliberate split: `[source] replace-with` = "identical content, different transport" (mirrors); `[registries.<name>]` + `[registry] default` = "different content, different identity" (private registries).
- Matching: exact source-name key, no wildcards, one-to-one.

### 4. Go module proxy (`GOPROXY`)
Doc: https://pkg.go.dev/cmd/go#hdr-Module_proxy_protocol

- `GOPROXY=https://proxy.corp,https://proxy.golang.org,direct` — ordered list.
- `404/410` = definitively absent, stop. Other errors (5xx/network) = fall through. `,` falls through only on absence; `|` falls through on any error. `direct` = VCS; `off` = no downloads.
- Companion globs: `GOPRIVATE`, `GONOSUMDB`, `GOINSECURE` (path-prefix patterns).
- Integrity: `sum.golang.org` checksum DB; per-pattern and global opt-outs.
- Richest *fallback* model — but fallback-to-internet is the **opposite** of a blocked-egress goal.

### 5. Maven `<mirror>` / `<mirrorOf>`
Doc: https://maven.apache.org/guides/mini/guide-mirror-settings.html

- `<mirrorOf>` patterns: `central` (exact), `*` (all), `external:*` (all but local/file), `repo1,repo2` (list), `*,!repo1` (all except), `external:*,!foo`.
- Exact match beats wildcard; declaration order matters for wildcards; **one mirror per repo, no fan-out**.
- Auth decoupled: separate `<server>` entry matched by `<id>`.
- Richest *selection* model.

### 6. npm `.npmrc`
Docs: https://docs.npmjs.com/cli/v11/configuring-npm/npmrc · https://docs.npmjs.com/cli/v11/using-npm/scope/

- Global `registry=` + per-scope `@scope:registry=`. Scope→registry one-to-one.
- Auth keyed **per host** (`//host/:_authToken=`), not per scope — one token covers all scopes resolving to that host.

### 7. Artifactory / Nexus / Harbor
Docs: https://docs.jfrog.com/artifactory/docs/docker-repositories · https://docs.jfrog.com/artifactory/docs/oci-repositories · https://help.sonatype.com/en/proxy-repository-for-docker.html · https://goharbor.io/docs/2.3.0/administration/configure-proxy-cache/

- Repo-key / project-name **path prefix is universal** (table above).
- Auth: standard OCI `Authorization: Bearer` to the proxy host (`docker login company.jfrog.io`).
- Integrity: proxy validates digest on cache fill and re-fetches on mismatch.

### 8. mise / aqua
Docs: https://mise.jdx.dev/registry.html · https://aquaproj.github.io/docs/reference/config/

- Neither has first-class OCI mirror/redirect config; they inherit OS/tool proxy env. Explained by their GitHub-Releases-first download model. A gap relative to container tooling — and an OCX differentiator opportunity.

## Synthesis & Recommendation for OCX

OCX constraints:
- **(a)** Canonical identifier immutability — `ocx.lock` stores upstream host + digest; the mirror must serve identical content; the identifier must not change.
- **(b)** Artifactory reality — the corporate mirror inserts a repo-key path prefix, so the client URL is structurally different from the upstream URL (host **and** path differ).
- **(c)** Existing config — OCX already has `[registry] default` + `[registries.<name>] url=`.

**Recommended model: Cargo `replace-with` *semantics* + containerd *per-upstream-host* keying + Artifactory *path-prefix* rewrite.**

- Key the mirror map by **upstream host** (containerd/`hosts.toml` directory-name analog), exact match.
- Rewrite **host + repository-path prefix** so an Artifactory path-method repo key works.
- **Replace semantics, first-match, no origin fallback** (Maven "first match, no fallback", *not* Go's comma-fallback). In a firewall-controlled environment, silent fallback to the open internet is the opposite of the goal — a failed mirror must error loudly.
- **Digest verification after every fetch** is the Cargo "identical content" rule realized via OCI content-addressing — it is what makes a transparent replacement safe. OCX already verifies (`verify_blob_digest`).
- Defer wildcard/`mirror_of` selection (Maven-style `*` / `!exclusion`) until a single-Artifactory-fronts-everything user appears (YAGNI); keep the schema open to it.

Suggested config shape (refined in the ADR):

```toml
[mirrors."ghcr.io"]
# Docker/OCI PULL path (`<host>/<repo-key>`), NOT the `/artifactory/api/docker/...`
# admin REST path (validated against current JFrog docs, 2026-06).
url = "https://company.jfrog.io/ghcr-remote"
# OCX appends the upstream repository path verbatim:
#   ghcr.io/owner/tool:1.2  ->  host company.jfrog.io,
#   repo ghcr-remote/owner/tool
```

**Why not containerd's ordered-fallback list for v1:** the corporate requirement is blocked egress (replace), so origin fallback is dead weight and a security footgun. **Why not the submodule (`external/rust-oci-client`) as the primary surface:** the submodule's value was connection-level fallback retry, which replace semantics removes; the rewrite becomes a one-shot pre-flight transform best done at the OCX client seam. (Full trade-off in the ADR.)

## Sources

- containerd hosts.md — https://github.com/containerd/containerd/blob/main/docs/hosts.md
- Cargo source replacement — https://doc.rust-lang.org/cargo/reference/source-replacement.html
- Cargo registries config — https://doc.rust-lang.org/cargo/reference/config.html#registries
- Go module proxy protocol — https://pkg.go.dev/cmd/go#hdr-Module_proxy_protocol
- Maven mirror settings — https://maven.apache.org/guides/mini/guide-mirror-settings.html
- Docker Hub mirror — https://docs.docker.com/docker-hub/image-library/mirror/
- npm scopes — https://docs.npmjs.com/cli/v11/using-npm/scope/
- npm .npmrc — https://docs.npmjs.com/cli/v11/configuring-npm/npmrc
- Artifactory Docker repos — https://docs.jfrog.com/artifactory/docs/docker-repositories
- Artifactory OCI repos — https://docs.jfrog.com/artifactory/docs/oci-repositories
- Harbor proxy cache — https://goharbor.io/docs/2.3.0/administration/configure-proxy-cache/
- Nexus Docker proxy — https://help.sonatype.com/en/proxy-repository-for-docker.html
- OCI Distribution Spec v1.1 — https://github.com/opencontainers/distribution-spec/blob/v1.1.0/spec.md
- mise registry — https://mise.jdx.dev/registry.html
- aqua config — https://aquaproj.github.io/docs/reference/config/
