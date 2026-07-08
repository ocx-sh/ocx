---
outline: deep
---

# Docker {#docker}

Official ocx container images live at `ghcr.io/ocx-sh/ocx`. Each image is the release binary from [GitHub Releases][releases] placed on a slim base — nothing is compiled in the image, so the `ocx` inside a tagged image is bit-identical to the archive of the same release. Images are published for `linux/amd64` and `linux/arm64` as multi-platform manifests, so `docker pull` resolves the right architecture automatically.

```sh
docker run --rm ghcr.io/ocx-sh/ocx ocx version
```

The images bake in [`OCX_NO_UPDATE_CHECK=1`][env-no-update-check]: the ocx version is pinned by the image tag, so the self-update notice would be pure noise.

## Images and Tags {#tags}

Two variants cover the practical spectrum. The default rides [Debian][debian] for glibc compatibility with whatever else your build installs; the [Alpine][alpine] variant carries the fully static musl build and stays small.

| Variant | Base image | Binary | Use when |
|---|---|---|---|
| `trixie` (default) | [`debian:trixie-slim`][docker-debian] | glibc (`*-unknown-linux-gnu`) | You add packages via `apt`, or run glibc-linked tools |
| `alpine` | [`alpine:3`][docker-alpine] | static musl (`*-unknown-linux-musl`) | Small images, or as a universal [`COPY --from`](#copy-from) source |

Tags follow the [Docker official images][docker-official] convention: version prefixes at every precision, with unsuffixed tags aliasing the default variant.

| Tag | Variant | Moves when |
|---|---|---|
| `0.9.2-trixie`, `0.9-trixie`, `0-trixie`, `trixie` | trixie | new release / weekly rebuild |
| `0.9.2-alpine`, `0.9-alpine`, `0-alpine`, `alpine` | alpine | new release / weekly rebuild |
| `0.9.2`, `0.9`, `0`, `latest` | trixie (alias) | new release / weekly rebuild |
| `0.9.2-trixie-20260708`, `0.9.2-alpine-20260708` | date-stamped | **never** — immutable |

All rolling tags — including the full `X.Y.Z` version tags — are re-pointed by a weekly rebuild that refreshes the base image, so security fixes in Debian or Alpine reach the images without waiting for the next ocx release. Only the latest release is rebuilt; older versions keep their tags on the base they shipped with.

::: info Silent rebuilds, like the official images
This is the [Docker official images][docker-official] model: `python:3.13-slim` also moves to a new digest when Debian patches something, without a Python release. A version tag means "this ocx version on the *current* base", not a frozen artifact.
:::

::: tip Reproducible builds pin the stamped tag or a digest
When a build must resolve the same bytes forever, use the date-stamped tag (`0.9.2-trixie-20260708`) or pin by digest (`ghcr.io/ocx-sh/ocx@sha256:…`). Stamped tags are never re-pointed, so digests they reference are never garbage-collected.
:::

## Copy the Binary {#copy-from}

The lightest way to use the images is not to base your image on them at all — copy the binary out, the way [uv documents][uv-docker] for its images:

```dockerfile
COPY --from=ghcr.io/ocx-sh/ocx:0.9.2-alpine /usr/local/bin/ocx /usr/local/bin/ocx
```

The `-alpine` variant is the right source for this: its binary is fully statically linked, so it runs on any base — Debian, Alpine, distroless, even `scratch`. The `trixie` variant's glibc binary works too, but only on glibc bases.

## Bake a Project Toolchain {#project-toolchain}

A project that pins its tools in [`ocx.toml` and `ocx.lock`][project-indepth] wants those exact tools inside its image. The wrong way is to resolve them when the container starts: every cold start then depends on registry availability, needs credentials at runtime, and can drift from what was tested. The right way is to pull at build time and run offline afterwards.

```dockerfile
FROM ghcr.io/ocx-sh/ocx:0.9.2

WORKDIR /app
# Lockfile layer first — Docker caches the pull until the lock changes.
COPY ocx.toml ocx.lock ./
RUN ocx pull

COPY . .
ENTRYPOINT ["ocx", "--offline", "run", "--", "task", "serve"]
```

[`ocx pull`][cmd-pull] walks the lockfile and downloads every digest-pinned tool for the image platform — it needs both files and touches nothing else, which makes it an ideal cache layer. [`ocx run`][cmd-run] composes the project environment and replaces itself with the child process, so no wrapper lingers in the process tree. [`--offline`][arg-offline] turns any accidental network dependency into a hard error at start instead of a silent pull — if the image builds, it runs.

## Reproducible Resolution {#frozen}

The [project-toolchain build](#project-toolchain) above runs a plain [`ocx pull`][cmd-pull], which resolves whatever a tag points to *now* — if the tag drifted since you wrote the lockfile, the build still succeeds, but with a version you never tested.

Swap in [`ocx --frozen pull`][arg-frozen] to close that gap. Frozen freezes tag→digest *resolution* to what the lockfile already pins: a tag missing from the lock **errors** instead of resolving fresh, so no unpinned version slips into the image. It is not a network ban — the digest-pinned blobs still download; frozen only refuses to *discover* a new mapping. Paired with the [`--offline`][arg-offline] run already in that Dockerfile, drift becomes a hard error at build time rather than a silent swap.

::: details Fully air-gapped builds
`--frozen pull` still reaches the registry for the digest-pinned blobs. For a build with no network at all, vendor a warm `OCX_HOME` into the build context — a directory populated by an earlier [`ocx pull`][cmd-pull] — and run the pull under [`--offline`][arg-offline], which resolves entirely from that local store and never touches the network.
:::

Resolving a bare tag like `cmake:3` deterministically *without* a lockfile — the [GitHub Actions][github-actions] and [Bazel][bazel] case — is a different tool: bundle a frozen index snapshot and point ocx at it with [`OCX_INDEX`][env-ocx-index]. See [Bundled Snapshots][indices-indepth].

## Private Registries {#build-auth}

`ocx pull` against a private registry needs credentials during `docker build`. Never bake them in with `ENV` or `ARG` — both persist in image history. Use [BuildKit secrets][buildkit-secrets], which mount for a single `RUN` and leave no layer behind:

```dockerfile
ENV OCX_AUTH_registry_example_com_TYPE=bearer
RUN --mount=type=secret,id=ocx_token,env=OCX_AUTH_registry_example_com_TOKEN \
    ocx pull
```

```sh
docker build --secret id=ocx_token,env=REGISTRY_TOKEN .
```

The variable name encodes the registry host with dots replaced by underscores (`registry.example.com` → `registry_example_com`); the auth type itself is not secret and can stay a plain `ENV`. See [`OCX_AUTH_<REGISTRY>_TOKEN`][env-auth-token] for the user/type companions.

## Bootstrap Single Tools {#mini-project}

To bootstrap a few tools in a Dockerfile without an application project, the same pattern shrinks to a minimal project: two files declaring the tools, and the pull happens at build time with the identical caching behavior.

```toml
# ocx.toml
[tools]
shellcheck = "ocx.sh/shellcheck:0.10"
shfmt = "ocx.sh/shfmt:3"
```

Run [`ocx lock`][cmd-lock] locally, commit both files, and the [project toolchain pattern](#project-toolchain) applies unchanged — `ocx run -- <cmd>` puts the tools on `PATH` for exactly that command. A more direct global-install story for Dockerfiles (persistent `PATH` without a project) is planned.

For CI pipelines that run *inside* these images — caching, matrix setups, and the full project-mode flow — see [CI Integration][ci-indepth].

<!-- external -->
[releases]: https://github.com/ocx-sh/ocx/releases/latest
[docker-official]: https://docs.docker.com/trusted-content/official-images/
[docker-debian]: https://hub.docker.com/_/debian
[docker-alpine]: https://hub.docker.com/_/alpine
[debian]: https://www.debian.org/
[alpine]: https://alpinelinux.org/
[uv-docker]: https://docs.astral.sh/uv/guides/integration/docker/
[buildkit-secrets]: https://docs.docker.com/build/building/secrets/
[github-actions]: https://docs.github.com/en/actions
[bazel]: https://bazel.build/

<!-- commands -->
[cmd-pull]: ./reference/command-line.md#pull
[cmd-run]: ./reference/command-line.md#run
[cmd-lock]: ./reference/command-line.md#lock

<!-- arguments -->
[arg-offline]: ./reference/command-line.md#arg-offline
[arg-frozen]: ./reference/command-line.md#arg-frozen

<!-- environment -->
[env-no-update-check]: ./reference/environment.md#ocx-no-update-check
[env-auth-token]: ./reference/environment.md#ocx-auth-registry-token
[env-ocx-index]: ./reference/environment.md#ocx-index

<!-- in depth -->
[project-indepth]: ./in-depth/project.md
[ci-indepth]: ./in-depth/ci.md
[indices-indepth]: ./in-depth/indices.md#bundled
