# Research: how other tools configure extra local sources and a writable cache

## Metadata

**Date:** 2026-11-22
**Lane:** prior-art scan (`/hex-discuss`, `.agents/discussions/bazel-hermetic-package-bundles.md`)
**Basis:** primary documentation as linked. Several fetches were truncated, so rows marked unverified were not confirmed from a primary source.
**Expires:** 2027-05-22
**Caveat:** findings only, no recommendation.

## Question

A package manager with a content-addressed local store wants extra local directories that are read before the network, and an opt-in writable cache that fills during a normal fetch. How do other tools express source order, read-only versus read-write, and the default write target? Do they use one mechanism for offline transfer and for CI caching, or two?

## Findings per tool

| Tool | Order or priority | Read-only versus read-write | Write target default | Extra-source layout versus own store | One or two mechanisms |
|---|---|---|---|---|---|
| Nix | `substituters` list, plus a per-store `priority` (lower wins, default 0, cache.nixos.org is 40) ([conf](https://nix.dev/manual/nix/latest/command-ref/conf-file), [file store](https://nix.dev/manual/nix/latest/store/types/local-binary-cache-store)) | Substituters are read-only. Writing is a separate act: `nix copy --to`, or `post-build-hook` ([nix copy](https://nix.dev/manual/nix/latest/command-ref/new-cli/nix3-copy)) | No automatic write. `post-build-hook` is opt-in. | The binary cache format (`file://`) differs from `/nix/store`. | One format serves both. `--offline` disables substituters. |
| Bazel | Disk cache plus remote cache. The combined semantics are documented only on third-party pages. `--distdir` is a separate source for archives ([flags](https://bazel.build/reference/command-line-reference)). | `--remote_upload_local_results=false` makes the remote read-only ([docs](https://bazel.build/remote/caching)). | Disk and remote are both read-write once flagged. | CAS plus action cache, separate from the repository cache. | Two: `--distdir` and `--repository_cache` for downloads, `--disk_cache` and `--remote_cache` for actions. |
| Go | Ordered `GOPROXY` list. `,` falls back on 404 and 410 only. `\|` falls back on any error. `direct` and `off` are keywords ([ref](https://go.dev/ref/mod)). | Not expressible. A proxy is read-only from `go`'s side. | `GOMODCACHE` is the single write target. | `cache/download` follows the proxy protocol, so a `file://` proxy can serve it as it is. | One layout serves both. |
| Cargo | `[source.X] replace-with = ...`. Replacement is one-to-one per source, not a list. | Not expressible. | Not applicable. | `local-registry` and `directory` are separate transfer formats ([docs](https://doc.rust-lang.org/cargo/reference/source-replacement.html)). | Offline transfer only. `~/.cargo/registry` is the cache. |
| pip | `--find-links` is added to the index. `--no-index` drops indexes. `--extra-index-url` is additive ([docs](https://pip.pypa.io/en/stable/cli/pip_install/)). | Read-only. | Its own cache is separate. | A flat directory of wheels and sdists, not the pip cache layout. | Two. |
| uv | `--find-links`, `--index`, `--default-index` and `--index-strategy` ([CLI](https://docs.astral.sh/uv/reference/cli/)). | `--offline` uses only local data. `--no-cache` neither reads nor writes. | `UV_CACHE_DIR`, always on. | A flat wheel directory, which is not the uv cache. | Two. |
| BuildKit | Repeated `--cache-from` flags. Order semantics are not stated. | Separate flags: `src=` to read, `dest=` to write ([docs](https://docs.docker.com/build/cache/backends/local/)). | Opt-in through `--cache-to`. | An OCI-layout directory. | One, for CI and for transfer. |
| ccache | Space-separated `remote_storage` list. The local cache is always queried first. Order among remotes is unspecified ([manual](https://ccache.dev/manual/latest.html)). | A per-entry `read-only` property. | Local is on by default. A remote is written on a miss unless `read-only`. | The `file:` backend layout is "similar (but not identical)" to local. `@layout=local` reads a mounted local layout. | One. |
| sccache | `SCCACHE_MULTILEVEL_CHAIN="disk,redis,s3"`, left to right ([config](https://github.com/mozilla/sccache/blob/main/docs/Configuration.md)). | Per backend: `SCCACHE_LOCAL_RW_MODE`, `SCCACHE_S3_RW_MODE=READ_ONLY`. | Depends on the chain. | Unverified. | One. |
| Gradle | Local and remote are separate blocks. Lookup order is not stated. | `push` per block. Remote defaults to `push = false` ([docs](https://docs.gradle.org/current/userguide/build_cache.html)). | Local is read-write. Remote needs `push = true`. | The directory cache is distinct from the HTTP remote. | One, CI-oriented. |
| Git | `objects/info/alternates`, one path per line. `GIT_ALTERNATE_OBJECT_DIRECTORIES` is `:`-separated, `;` on Windows ([layout](https://git-scm.com/docs/gitrepository-layout), [env](https://git-scm.com/docs/git)). | Alternates are read-only: "New objects will not be written to these directories". | The repository's own `objects/`. | Identical to the repository's own object store. | One. |
| XDG | `XDG_DATA_DIRS` is ordered, first is most important. `XDG_DATA_HOME` outranks all of them ([spec](http://specifications.freedesktop.org/basedir/latest/)). | `XDG_DATA_HOME` is written. `XDG_DATA_DIRS` is search only. | `XDG_DATA_HOME`, always. | Not applicable. | Not applicable. |
| Spack | Mirror ordering unverified. | `--autopush` and `--no-autopush` ([docs](https://spack.readthedocs.io/en/latest/binary_caches.html)). | Opt-in: push only through `buildcache push` or `--autopush`. | A `v3/` plus `blobs/<algo>/` tree, the same for a local path and a URL. | One mirror concept serves both. |
| pnpm | Not applicable. | `--frozen-store` opens the store read-only. `--offline` forbids the network ([docs](https://pnpm.io/cli/install)). | The store, always. | It is the store. | Not applicable. |

## Patterns

1. **An ordered read list plus one fixed write target** (Git, XDG, Go `GOMODCACHE`, Nix substituters, uv, pip).
   - For: the write target is never ambiguous.
   - Against: the read list cannot be written through. Nix needs `nix copy` or a hook to write.
2. **One ordered list with per-entry attributes** (ccache `read-only`, Nix `priority`, sccache `rw_mode`).
   - For: one place to express mode and trust per source.
   - Against: ccache states that order among remote entries is unspecified. Nix uses a numeric priority instead of list position.
3. **Separate read and write keys or blocks** (Gradle `push`, BuildKit `src` and `dest`, Bazel `--remote_upload_local_results`).
   - For: the write side is opt-in by default.
   - Against: the order across sources is implicit or undocumented.
4. **The tool's own store layout reused as the extra source** (Git, Go `cache/download`, ccache `@layout=local`, Spack).
   - For: no export step.
   - Against: it needs a layout stable enough to share.
5. **A separate transfer format** (Cargo, pip `--find-links`, Nix binary cache).
   - For: the export is decoupled from the cache internals.
   - Against: an extra export tool is needed.
6. **A writable cache grows without bound unless something prunes it.**
   - BuildKit's local backend has [no automatic pruning](https://docs.docker.com/build/cache/backends/local/) except `reset=true` on Buildx 0.35.0 or later.
   - ccache has no cleanup for `file:` storage; the manual points at `ccache --trim-dir`.
   - Bazel gained disk-cache garbage-collection flags only in 7.4 ([docs](https://bazel.build/remote/caching)).

## Other points

- **Fallback semantics.** Go has two fallback modes in one list: `,` for "not found" and `|` for "any error".
- **Gaps.** Not confirmed from a primary source: Bazel's `--repository_cache` default location, the Nix `?priority=` URL-query syntax, Spack's `mirrors.yaml` ordering.
- **Not covered.** Homebrew bottle caches, the npm cache, Nix `trusted-substituters`, the Cargo registry cache layout.
