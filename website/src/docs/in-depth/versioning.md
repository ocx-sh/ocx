---
outline: deep
---
# Versioning

Every binary package has three dimensions: *what it is*, *which version*, and *which platform build*. Most ecosystems handle these separately — apt with [arch suffixes and ports mirrors][apt-ports], pip with [platform-encoded wheel filenames][pep-491], Bazel rules with [hand-curated URL matrices][toolchains-llvm] — and leave the coordination to the consumer.

OCX collapses all three dimensions into a single identifier. `cmake:3.28` resolves the right binary for the current machine, verifies it cryptographically, and installs it — without flags, name suffixes, or mirror configuration. This page explains *how* tags, variants, and platforms compose under the hood. The user-facing surface — pick a tag, pin a digest, override platform — lives in the [Versioning section of the user guide][user-versioning].

## Tags {#tags}

An OCI tag is a human-readable label pointing to a specific manifest. The [OCI Distribution Specification][oci-dist-tag] offers no notion of tag stability — tags are mutable pointers. Structure and stability emerge from publishing conventions.

Rolling tags are a widespread pattern in container ecosystems — Docker [official images][docker-images] use them extensively (`ubuntu:24.04`, `node:22`, `nginx:latest`). [Semantic Versioning][semver] formalizes the same hierarchy for software releases. OCX adopts both conventions for binary packages.

OCX follows a semver-inspired hierarchy where specificity signals intent:

```
{major}[.{minor}[.{patch}[-{prerelease}][_{build}]]]
```

The `_build` suffix — typically a UTC timestamp like `_20260216120000` — signals that the publisher considers the tag final and does not intend to re-push it. It is a *convention*, not a guarantee enforced by the registry.

::: tip Why `_` instead of semver's `+`?
The [OCI Distribution Specification][oci-dist-tag] restricts tags to `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}` — the `+` character is not allowed. OCX uses `_` as the build separator so every version string is a valid OCI tag by construction. This follows the same convention adopted by [Helm][helm-oci] for OCI-stored charts.

If you type `+` out of habit (e.g., `cmake:3.28.1+20260216`), OCX accepts it and normalizes to `_` automatically. See [Build Separator][faq-build-separator] in the FAQ for the full rationale.
:::

| Tag | Publisher's intent | After index refresh |
|---|---|---|
| `cmake:3.28.1_20260216120000` | Do not re-push | Same build (by publisher convention) |
| `cmake:3.28.1` | Rolling patch | Latest build of 3.28.1 |
| `cmake:3.28` | Rolling minor | Latest 3.28.x build |
| `cmake:3` | Rolling major | Latest stable 3.x build |
| `cmake:latest` | Floating | Latest release |

::: warning OCI tags are not immutable
Any tag — including build-tagged ones — can be overwritten by the publisher. Two mechanisms protect installs regardless of what the registry does later:

- **[Local tag store][in-depth-indices] snapshot.** The tag → digest mapping only changes when [`ocx index update`][cmd-index-update] runs. The snapshot is yours until you decide to refresh it.
- **[Content-addressed package store][in-depth-storage-packages].** Once a binary is installed, it lives at a path derived from its SHA-256 digest. The tag used to install it is irrelevant — the bytes are permanent.

For absolute reproducibility without any index, reference the digest directly: `cmake@sha256:abc123…`
:::

## Variants {#variants}

A binary tool can be built multiple ways — different optimization profiles (`pgo`, `pgo.lto`), feature toggles (`freethreaded`), or size trade-offs (`slim`). Variants describe *how* a binary was built, not *where* it runs. All variants for a given platform execute on the same host; the user chooses the variant, it is not auto-detected.

Without variant support, publishers create separate packages (`python-pgo`, `python-debug`) to represent these build differences, which loses the semantic connection between builds of the same tool.

::: details Why not Docker's tag-suffix convention?
[Docker Hub][docker-images] encodes variants as tag suffixes (`python:3.12-slim`), but this creates a parsing ambiguity with [semver][semver] prereleases — `3.12-alpha` and `3.12-slim` are syntactically indistinguishable. [Concourse CI][concourse-registry]'s registry image resource documents this explicitly, resorting to a heuristic: only `alpha`, `beta`, and `rc` are treated as prereleases, everything else is a variant suffix. OCX avoids this ambiguity entirely with a variant-*prefix* format.
:::

OCX uses a <Tooltip term="variant-prefix format">The variant name comes before the version, separated by a hyphen: `debug-3.12.5`. Because variants start with a letter and versions start with a digit, the boundary is always unambiguous. Variant names match `[a-z][a-z0-9.]*` — lowercase letters, digits, and dots only.</Tooltip> convention. The default variant's tags carry no prefix — `python:3.12` resolves the same build as `python:pgo.lto-3.12` when `pgo.lto` is the default.

| Tag | Meaning |
|---|---|
| `python:3.12` | Default variant at version 3.12 |
| `python:debug-3.12` | Debug variant at version 3.12 |
| `python:debug-3` | Latest version of the debug variant in the 3.x series |
| `python:debug` | Latest version of the debug variant |
| `python:latest` | Latest version of the default variant |

Rolling tags cascade within their variant track: `debug-3.12.5` cascades to `debug-3.12` → `debug-3` → `debug`. Variants never cross — publishing a new `debug` build never updates `pgo.lto` or the default variant's tags. See [Cascades](#cascades) for the full cascade model.

For the default variant, cascading also produces unadorned alias tags: publishing a new default-variant build cascades both the prefixed track (if any) and the bare `3.12.5` → `3.12` → `3` → `latest` track.

::: warning Platform vs. variant
[OCI `platform.variant`][oci-image-index] is a CPU sub-architecture field (ARM `v6`, `v7`, `v8`) — it determines *where* a binary can run and is selected automatically. OCX software variants determine *how* a binary was built and are selected by the user. The two concepts are orthogonal.
:::

## Cascades {#cascades}

Publishers are expected to maintain the full tag hierarchy. When `cmake:3.28.1_20260216120000` is released, all rolling ancestors are re-pointed — but only if this is genuinely the latest at each specificity level:

```
cmake:latest                  ← rolling — updated
       ↓
cmake:3                       ← rolling — updated
       ↓
cmake:3.28                    ← rolling — updated
       ↓
cmake:3.28.1                  ← rolling — updated
       ↓
cmake:3.28.1_20260216120000   ← source of truth
```

Publishing `cmake:3.27.5_20260217` would update `cmake:3.27` but not `cmake:3` or `cmake:latest` — the `3.28.x` series is still ahead. Rolling tags only advance, never regress.

This is a publishing convention, not a guarantee enforced by the registry. Publishers must maintain the cascade manually — or have OCX do it for them.

Cascades operate within a single variant track. Publishing `debug-3.28.1` updates `debug-3.28`, `debug-3`, and `debug` — but never touches the default variant's tags (`3.28`, `3`, `latest`) or any other variant's tags. For the default variant, cascading also produces unadorned alias tags that mirror the variant-prefixed chain. See [Variants](#variants) for details.

::: tip Cascading a new release
[`ocx package push --cascade`][cmd-package-push] handles the full cascade automatically: publish one build and let OCX re-point all rolling ancestors in a single command.
:::

## Platforms {#platforms}

The [OCI Image Index specification][oci-image-index] defines a <Tooltip term="multi-platform manifest">An image index is a top-level OCI manifest that lists multiple per-platform sub-manifests under a single tag. Clients resolve the entry that matches their platform; the tag and repository name stay the same across all platforms.</Tooltip> format that allows multiple platform builds to live under a single tag. When you install a package, OCX reads your running system and selects the matching build automatically — no flags, no configuration, no separate repository per architecture.

| System | Detected as |
|---|---|
| Linux on x86-64 | `linux/amd64` |
| Linux on ARM (Graviton, Ampere, RPi 4+) | `linux/arm64` |
| macOS on Intel | `darwin/amd64` |
| macOS on Apple Silicon | `darwin/arm64` |
| Windows on x86-64 | `windows/amd64` |

If a package publishes a universal build with no platform field in the manifest, OCX uses it as a fallback — common for interpreted runtimes and truly portable binaries.

### Richer platform descriptors {#platform-fields}

The OCI platform model supports finer-grained descriptors for precise compatibility targeting:

- **`variant`** — CPU sub-architecture. For ARM: `v6`, `v7` (32-bit), `v8` (64-bit / arm64). OCX selects the most specific match.
- **`os.version`** — OS version string, primarily used for Windows Server editions (e.g. `10.0.14393.1066`).
- **`os.features`** — OS capability flags, e.g. `win32k` for Windows container isolation modes.
- **`features`** — Reserved for future platform extensions in the OCI spec.

OCX matches all declared fields when selecting among manifest entries. See the [OCI Image Index specification][oci-image-index] for the complete field reference.

::: warning OCI platform.variant ≠ OCX variants
[`platform.variant`][oci-image-index] is a CPU sub-architecture field, distinct from [OCX software variants](#variants), which describe build-time characteristics like optimization profiles or feature sets.
:::

## Locking {#locking}

The most direct lock is a digest: `cmake@sha256:abc123…` bypasses the [tag store][in-depth-indices] entirely and identifies an exact binary regardless of what any tag points to. Because all installed bytes are content-addressed, every package can be pinned this way — no lockfiles, no registry queries, just the hash.

For most use cases, the [local tag store snapshot][in-depth-indices] already provides the lock. Tags resolve to the digest recorded at last update, and that mapping does not change until [`ocx index update`][cmd-index-update] runs. A CI runner that never updates its tag store gets the same binary on every run.

For tool authors distributing OCX-powered workflows, there is a more ergonomic option. The local tag store holds only metadata — small JSON files, no binaries — so it can be shipped inside a [GitHub Action][github-actions-docs], [Bazel Rule][bazel-rules], or [DevContainer Feature][devcontainer-features]. The result is a two-level lock: the *tool version* locks the *tag store snapshot*, which locks the *resolved binary*. Users pin the tool and get deterministic builds without managing digests, platform conditionals, or separate lockfiles.

The full pattern — bundled snapshots, [`OCX_INDEX`][env-ocx-index], `--remote` and `--offline` interaction, Dependabot/Renovate flow — lives in [Indices][in-depth-indices].

## See Also

- [Versioning section in the user guide][user-versioning] — how-to: pick a tag, pin a digest, override platform
- [Indices][in-depth-indices] — tag-store snapshots, bundled index pattern, lock semantics
- [Storage][in-depth-storage] — content-addressed package store, layer dedup
- [Build Separator FAQ][faq-build-separator] — full rationale for `_` over `+`

<!-- external -->
[oci-dist-tag]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-manifests
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[semver]: https://semver.org/
[docker-images]: https://hub.docker.com/search?image_filter=official
[helm-oci]: https://helm.sh/docs/topics/registries/
[concourse-registry]: https://github.com/concourse/registry-image-resource
[apt-ports]: https://wiki.debian.org/Multiarch/HOWTO
[pep-491]: https://peps.python.org/pep-0491/
[toolchains-llvm]: https://github.com/bazel-contrib/toolchains_llvm/blob/master/toolchain/internal/llvm_distributions.bzl
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[bazel-rules]: https://bazel.build/extending/rules
[devcontainer-features]: https://containers.dev/implementors/features/

<!-- commands -->
[cmd-package-push]: ../reference/command-line.md#package-push
[cmd-index-update]: ../reference/command-line.md#index-update

<!-- environment -->
[env-ocx-index]: ../reference/environment.md#ocx-index

<!-- internal -->
[user-versioning]: ../user-guide.md#versioning
[in-depth-storage]: ./storage.md
[in-depth-storage-packages]: ./storage.md#packages
[in-depth-indices]: ./indices.md
[faq-build-separator]: ../faq.md#versioning-build-separator
