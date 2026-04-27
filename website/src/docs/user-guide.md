---
outline: deep
---
# User Guide

## File Structure {#file-structure}

Most package managers store everything in a single mutable tree — installing a new version silently replaces the old one, breaking anything that referenced the old path. They also hit the network on every operation, making offline use and reproducible CI awkward.

### Storage

ocx separates concerns into independent stores under `~/.ocx/` (configurable via [`OCX_HOME`][env-ocx-home]):

<Tree>
  <Node name="~/.ocx/" icon="🏠" open>
    <Node name="packages/" icon="📦">
      <Description>immutable, content-addressed assembled packages</Description>
    </Node>
    <Node name="layers/" icon="🗂️">
      <Description>extracted OCI layers — shared across packages that reference the same layer</Description>
    </Node>
    <Node name="blobs/" icon="📋">
      <Description>raw OCI blobs — manifests, image indexes, referrers</Description>
    </Node>
    <Node name="tags/" icon="🏷️">
      <Description>local mirror of registry tag-to-digest mappings — no binaries</Description>
    </Node>
    <Node name="symlinks/" icon="🔀">
      <Description>stable symlinks safe to embed in shell profiles and configs</Description>
    </Node>
    <Node name="temp/" icon="🧪">
      <Description>download staging — cleaned on successful install</Description>
    </Node>
  </Node>
</Tree>

Each store has a single responsibility. Upgrading a package updates symlinks in `symlinks/` and adds an entry to `packages/`, but never touches `tags/`. The stores can be understood — and reasoned about — one at a time.

::: details Path component encoding
Registry names, repository names, and tags that appear as directory or file names in the stores are <Tooltip term="slugified">Characters outside `[a-zA-Z0-9._-]` are replaced with underscores. For example, `localhost:5000` becomes `localhost_5000`. This prevents the POSIX PATH separator (`:`) from corrupting environment variables that embed these paths.</Tooltip> before they become filesystem path components. Dots and hyphens are preserved so that domain names (e.g. `ghcr.io`) and semantic versions (e.g. `3.28.1`) remain readable on disk.

Digest-addressed directories use a two-level shard (`sha256/{2hex}/{30hex}/`). Only 32 hex characters are encoded in the path; the full digest is written to a sibling `digest` file inside each entry and is the source of truth for identity.
:::

#### Packages {#file-structure-packages}

When ocx installs a package, the actual files land in `packages/`. The critical design decision: the storage path is derived from the content's <Tooltip term="SHA-256 digest">A 64-character hex fingerprint of the exact bytes in a file or archive. The same bytes always produce the same digest; any change — even a single bit — produces a completely different one. Used here as a content address: the path is uniquely determined by what the files contain.</Tooltip>, not from the package name or tag.

<Tree>
  <Node name="~/.ocx/packages/" icon="📦" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="sha256/ab/c123…/" icon="📁" open-icon="📂" open>
        <Description>one directory per unique package build</Description>
        <Node name="content/" icon="📂">
          <Description>package files — binaries, libraries, headers</Description>
        </Node>
        <Node name="metadata.json" icon="📋">
          <Description>declared env vars and extraction options</Description>
        </Node>
        <Node name="refs/" icon="🔗">
          <Description>back-references to install symlinks — guards against deletion</Description>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

This <Tooltip term="content-addressed layout">A storage scheme where every item's address is derived from a cryptographic hash of its contents. The same bytes always map to the same address — accidental duplication is impossible and silent corruption is detectable.</Tooltip> has two important consequences:

- **Automatic deduplication.** If `cmake:3.28` and `cmake:latest` resolve to the same binary build, they share one directory on disk. Storage is proportional to the number of *distinct builds*, not the number of tags that reference them.
- **Immutability.** A path under `sha256/<shard>/` never changes its contents. This makes packages safe to reference directly from scripts or cache layers that require a known, stable binary.

::: info Similar to the Nix store and Git objects
The [Nix package manager][nix] stores every package at `/nix/store/{hash}-name/` using the same principle: the path is a function of the content. This is what makes Nix derivations reproducible across machines — the same hash always means the same files. Git's internal object store (`.git/objects/`) works identically. ocx applies this model to OCI-distributed binaries.
:::

**Garbage collection via `refs/`.** The `refs/symlinks/` subdirectory inside each package tracks every install symlink that currently points to it. That directory is the GC root signal — [`ocx clean`][cmd-clean] starts a reachability walk from every package with a live `refs/symlinks/` entry and follows forward-refs through all three tiers.

::: details How back-references work
When `ocx install cmake:3.28` creates the symlink `symlinks/…/cmake/candidates/3.28 → packages/…/sha256/ab/c123…/content`, it simultaneously writes a back-reference entry inside the package's `refs/symlinks/` directory. Removing the symlink via [`ocx uninstall`][cmd-uninstall] removes that back-reference entry. [`ocx clean`][cmd-clean] then builds a reachability graph across all three tiers: packages with live `refs/symlinks/` entries (and any profile content-mode references) are roots, and a single BFS pass follows each package's forward-refs in `refs/deps/`, `refs/layers/`, and `refs/blobs/`. Packages, layers, and blobs that remain unreachable across all three tiers are deleted in one sweep.
:::

*Commands: [`ocx install`][cmd-install] adds packages; [`ocx uninstall --purge`][cmd-uninstall] removes a specific one; [`ocx clean`][cmd-clean] removes all unreferenced packages.*

#### Layers {#file-structure-layers}

A package on disk looks monolithic — one `content/` directory under one digest — but the bytes inside it can come from more than one upstream archive. Each archive is a <Tooltip term="layer">An OCI image layer: a single tar archive (gzip or xz compressed) addressed in the registry by its own SHA-256 digest. The OCI image manifest lists all layers that compose a package; on pull, ocx extracts each layer once into the shared layer store and assembles them into the package's `content/` directory via hardlinks.</Tooltip>, stored once in `~/.ocx/layers/` and shared across every package that references it.

<Tree>
  <Node name="~/.ocx/layers/" icon="🗂️" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="sha256/ab/c123…/" icon="📁" open-icon="📂" open>
        <Description>one directory per unique layer blob</Description>
        <Node name="content/" icon="📂">
          <Description>extracted layer files — hardlink source for assembled packages</Description>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

The layer store enables a different kind of dedup than the package store. The package store dedups whole builds — two tags pointing at the exact same binary share one directory. The layer store dedups *parts* of builds: a 200 MB shared base layer used by ten packages is downloaded, extracted, and stored exactly once. Each package's `content/` is then assembled by hardlinking files from one or more layer directories, so the package looks complete even though no bytes were copied.

::: info Similar to Docker image layers and pnpm's content store
[Docker image layers][docker-layers] are the same concept at the registry level: an image is a stack of layers, each addressed by digest, downloaded only when missing from the local cache. ocx applies this to binary packages instead of containers. [pnpm][pnpm] uses a comparable trick on the install side, storing every package version once in a content-addressed store and hardlinking them into individual `node_modules/` trees.
:::

**Publishing multi-layer packages.** When you call [`ocx package push`][cmd-package-push], every positional argument after the identifier is a layer — zero or more. Each layer is either a path to a local archive file or a digest reference to a layer that already exists in the target registry. A push with zero layers is valid too: it produces a config-only OCI artifact (useful for referrer-only / description-only manifests) and requires `--metadata` since there is no file layer to sniff a sibling metadata path from.

```shell
# Two-layer package: shared base + package-specific top
ocx package push -p linux/amd64 mytool:1.2.3 base.tar.gz tool.tar.gz

# Re-publish with the base layer reused by digest — no re-upload.
# The digest ref must spell out the original archive extension
# (`.tar.gz` / `.tgz` / `.tar.xz` / `.txz`) — OCI blob HEADs do not
# carry the media type, so ocx refuses to guess.
ocx package push -p linux/amd64 mytool:1.2.4 sha256:<hex>.tar.gz newtool.tar.gz
```

The order matters for the manifest descriptor list, but assembled content must not overlap — two layers cannot contain the same file path. Overlap is rejected at install time with a clear error.

::: info Digest verification on pull
Every layer blob downloaded by `ocx install` or `ocx package pull` is streamed through SHA-256 on the way to disk and compared against the digest declared in the manifest before extraction. A mismatch — the registry serving different bytes for the same digest ([CWE-345](https://cwe.mitre.org/data/definitions/345.html)) — deletes the tampered file and fails the command. Zero-layer pulls are valid: a config-only package (produced by `ocx package push` with no file layers and `--metadata`) installs into an empty `content/` directory, which is the expected shape for referrer-only or description-only artifacts.
:::

::: warning Bring your own archives
`ocx package push` does **not** bundle a directory for you. Each layer must be a pre-built archive (`.tar.gz` / `.tgz` or `.tar.xz` / `.txz`). This is intentional: archive creation is non-deterministic (timestamps, compression entropy, file ordering), so re-bundling the same content yields a different digest and defeats layer reuse. Use [`ocx package create`][cmd-package-create] if you need to bundle a directory — that command produces a stable archive once, which you can then push and reference by digest from any number of subsequent packages.

If a file in your current directory is literally named like a digest reference (e.g. `sha256:abc….tar.gz`), prefix it with `./` to force file interpretation — bare `sha256:…` tokens are always parsed as digest refs.
:::

::: tip Designing for reuse
The layer reuse workflow is most valuable when you have many packages that share a large common base — a runtime library, a vendored toolchain, or a fixed dataset. Push the base once, record its digest, and reference it from every dependent package's push command. The registry stores it once; ocx downloads it once; every package gets a fresh `content/` view assembled from the same shared layer.
:::

*Commands: [`ocx package push`][cmd-package-push] publishes layered packages; [`ocx package create`][cmd-package-create] bundles a directory into a stable archive.*

#### Tags {#file-structure-tags}

When you run `ocx install cmake:3.28`, how does ocx know which binary to fetch? It looks up the tag `3.28` in the local tag store and finds the corresponding <Tooltip term="digest">The content fingerprint stored in the OCI registry manifest. A tag like `3.28` is just a human-readable alias; the digest is the canonical, immutable identifier that pinpoints the exact binary build behind that tag at index-update time.</Tooltip>. The tag store is a local copy of that mapping — no network required.

<Tree>
  <Node name="~/.ocx/tags/" icon="🏷️" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="{repo}.json" icon="📄">
        <Description>{ "3.28": "sha256:abc…", "3.30": "sha256:def…" }</Description>
      </Node>
    </Node>
  </Node>
</Tree>

The tag store is a *snapshot*: it reflects the state of the remote registry at the last time you refreshed it. That snapshot has two benefits:

- **Offline installs.** If the tag store is populated and the package is already in `packages/`, `ocx install cmake:3.28` works with no network access at all — tag resolution is a local file read.
- **Reproducibility.** A CI runner that does not update the tag store gets the same binary on every run, regardless of what the registry serves today. Tags in OCI registries are mutable; your local snapshot is not.

::: info Similar to APT's package lists
`apt-get update` downloads package metadata from configured sources and caches it in `/var/lib/apt/lists/`. All subsequent `apt-get install` calls resolve packages from that local snapshot — the network is only involved during an explicit refresh, not on every install. `ocx index update <package>` is the per-package equivalent: you control when the snapshot changes, and the rest of the time you work from the local cache.
:::

`ocx index update <package>` refreshes the tag store for a specific package. The global flag [`--remote`][arg-remote] forces tag and catalog lookups to query the registry directly for a single command, without updating the persistent local tag snapshot. Blob data fetched under `--remote` still populates `$OCX_HOME/blobs/` via write-through, so subsequent offline installs work for any package that was resolved while online.

On a fresh machine, you do not need to run [`ocx index update`][cmd-index-update] before the first [`ocx install cmake:3.28`][cmd-install]. When the local tag store has no entry for a requested tag, [`ocx install`][cmd-install] transparently resolves that single tag against the configured remote, persists it to the tag store, and proceeds with the install. Subsequent commands — including [`--offline`][arg-offline] — then work from the cached entry without touching the network. Refreshing a cached tag or discovering every tag for a repository is still the job of [`ocx index update`][cmd-index-update]; the fallback only covers the specific tag being installed.

*Commands: [`ocx index catalog`][cmd-index-catalog], [`ocx index update`][cmd-index-update]; flag: [`--remote`][arg-remote]*

#### Symlinks {#file-structure-symlinks}

Package paths embed the digest: `~/.ocx/packages/ocx.sh/sha256/ab/c123…/content`. That path changes on every upgrade. You cannot put it in a shell profile, an IDE config, or a build file and expect it to still work next month.

`symlinks/` solves this with <Tooltip term="stable symlinks">A symbolic link (symlink) is a filesystem entry that points to another path. The link's own path never changes — only its target can be updated. Tools that reference the link path continue to work transparently after the target is re-pointed to a new version.</Tooltip> whose paths never change — only their targets are re-pointed when you install or select a new version.

<Tree>
  <Node name="~/.ocx/symlinks/" icon="🔀" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="{repo}/" icon="📁" open-icon="📂" open>
        <Node name="current" icon="➡️">
          <Description>active version — set by ocx select</Description>
        </Node>
        <Node name="candidates/" icon="📁" open-icon="📂" open>
          <Node name="3.28" icon="➡️">
            <Description>pinned install — created by ocx install cmake:3.28</Description>
          </Node>
          <Node name="3.30" icon="➡️">
            <Description>pinned install — created by ocx install cmake:3.30</Description>
          </Node>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

There are two symlink levels, for two different use cases:

**`candidates/{tag}`** — pinned to a specific version. Created by `ocx install` and pointed at the exact digest that tag resolved to at install time. You can have cmake 3.28 and 3.30 installed side by side; both candidates coexist until you explicitly uninstall one. Even if the registry later re-pushes the `3.28` tag with a different binary, your candidate still points to the build you originally installed.

**`current`** — a floating pointer to whichever candidate you last declared active. Set by [`ocx select`][cmd-select] (or `ocx install --select` in one step). It is never updated automatically — not when you install a newer version, not when you update the tag store. This is intentional: tools referencing `current` should only change behavior when *you* decide they should.

::: info Inspired by SDKMAN and Homebrew
[SDKMAN][sdkman] (the Java SDK manager) uses the same two-level pattern: `~/.sdkman/candidates/{tool}/{version}/` for pinned installs and a `current` symlink updated by `sdk default {version}`. [Homebrew][homebrew] does the same with its `Cellar/{formula}/{version}/` store and a stable `opt/{formula}` symlink pointing at the active version. Linux's `update-alternatives` is the system-level equivalent, managing tools like `java` and `python3` via a layer of stable symlinks in `/etc/alternatives/`.
:::

::: tip Stable paths in IDE settings and shell profiles
Because `current` never changes its own path, you reference it once and forget it:

```shell
# Install cmake and make it the active version in one step
ocx install --select cmake:3.30
```

```jsonc
// .vscode/settings.json — path survives every future upgrade
{ "cmake.cmakePath": "~/.ocx/symlinks/ocx.sh/cmake/current/content/bin/cmake" }
```

```sh
# ~/.bashrc — always resolves to the selected version
export PATH="$HOME/.ocx/symlinks/ocx.sh/cmake/current/content/bin:$PATH"
```

When you later run `ocx install --select cmake:3.32`, `current` is re-pointed. Your IDE and shell pick up the new version automatically — no config changes needed.
:::

*Commands: [`ocx install`][cmd-install], [`ocx select`][cmd-select]; [`ocx deselect`][cmd-deselect], [`ocx uninstall`][cmd-uninstall]*

### Path Resolution {#path-resolution}

Several commands resolve package content to a filesystem path that is embedded in their output.
The path you receive determines how stable that output is across future package updates.

| Mode | Flag | Path used | Auto-install | Use case |
|---|---|---|---|---|
| Package store *(default)* | *(none)* | `~/.ocx/packages/…/<digest>/content` | yes (online) | CI, scripts, one-shot queries |
| Candidate symlink | `--candidate` | `~/.ocx/symlinks/…/candidates/<tag>/content` | **no** | Pinning to a specific tag in editor or IDE config |
| Current symlink | `--current` | `~/.ocx/symlinks/…/current/content` | **no** | "Always selected" path in shell profiles or IDE settings |

**Package store** paths are content-addressed and change whenever the package digest changes (i.e. on every update).
They are precise and self-verifying but unsuitable for static configuration.

**Candidate** and **Current** paths go through symlinks managed by ocx.
Because the symlink path itself does not change, any tool that hardcodes the path continues to work after the package is updated — only the symlink target is re-pointed.

- `--candidate` requires the package to be installed (the candidate symlink must exist).
  A digest component in the identifier is rejected; use a tag-only identifier.
- `--current` requires a version of the package to be selected (the current symlink must exist),
  mirroring the `update-alternatives` model on Linux.
  `current` is not automatically the latest installed version — it only moves when explicitly selected.
  A digest component in the identifier is rejected.

Both symlink modes fail immediately if the required symlink is absent.
They never attempt to install or select a package as a side effect.

::: tip Example — stable path for VSCode `settings.json`
```jsonc
{
  // This path remains valid across package updates — only the symlink target changes.
  "clangd.path": "/home/user/.ocx/symlinks/ocx.sh/clangd/current/content/bin/clangd"
}
```

Set it up once by installing and selecting a version, then inspect the stable path:
```shell
ocx env --current clangd
```
:::

Use [`ocx find`][cmd-find] to print the resolved content path directly, without setting environment variables. Supports the same `--candidate` and `--current` modes and is useful for scripting.

## Versioning {#versioning}

Every binary package has three dimensions: *what it is*, *which version*, and *which platform build*. Most tools handle these separately — different flags, different naming conventions, different configuration layers — and leave the coordination to you.

Each ecosystem encodes platform differently: apt uses <Tooltip term="arch suffixes and ports mirrors">`dpkg --add-architecture arm64`, a `ports.ubuntu.com` mirror in `sources.list.d`, then `libssl-dev:arm64` — three steps before any package downloads</Tooltip>, pip bakes the platform into <Tooltip term="the wheel filename">`cryptography-41.0.3-cp311-cp311-manylinux_2_28_aarch64.whl` — fetching the arm64 variant requires five explicit flags</Tooltip>, and Bazel rules tend to maintain a <Tooltip term="hand-curated URL matrix">one `filename → checksum` entry per `version × os × arch`, like the Bazel LLVM toolchain's hundreds of entries updated on every release</Tooltip>. Version and platform are always managed as separate concerns.

ocx collapses all three dimensions into a single identifier. Platform is auto-detected and applied silently; version semantics are expressed through the tag convention. `ocx install cmake:3.28` resolves the right binary for the current machine, verifies it cryptographically, and installs it — without flags, name suffixes, or mirror configuration.

### Tags {#versioning-tags}

An OCI tag is a human-readable label pointing to a specific manifest. The OCI specification offers no notion of tag stability — tags are mutable pointers. Structure and stability emerge from publishing conventions.

Rolling tags are a widespread pattern in container ecosystems — Docker [official images][docker-images] use them extensively (`ubuntu:24.04`, `node:22`, `nginx:latest`). [Semantic Versioning][semver] formalizes the same hierarchy for software releases. ocx adopts both conventions for binary packages.

ocx follows a semver-inspired tag hierarchy where specificity signals intent:

```
{major}[.{minor}[.{patch}[-{prerelease}][_{build}]]]
```

The `_build` suffix — typically a UTC timestamp like `_20260216120000` — signals that the publisher considers the tag final and does not intend to re-push it. It is a convention, not a guarantee enforced by the registry.

::: tip Why `_` instead of semver's `+`?
The OCI Distribution Specification restricts tags to `[a-zA-Z0-9_][a-zA-Z0-9._-]{0,127}` — the `+` character is not allowed. ocx uses `_` as the build separator so that every version string is a valid OCI tag by construction. This follows the same convention adopted by [Helm][helm-oci] for OCI-stored charts.

If you type `+` out of habit (e.g., `cmake:3.28.1+20260216`), ocx accepts it and normalizes to `_` automatically. See [Build Separator][faq-build-separator] in the FAQ for the full rationale.
:::

| Tag | Publisher's intent | After index refresh |
|---|---|---|
| `cmake:3.28.1_20260216120000` | Do not re-push | Same build (by publisher convention) |
| `cmake:3.28.1` | Rolling patch | Latest build of 3.28.1 |
| `cmake:3.28` | Rolling minor | Latest 3.28.x build |
| `cmake:3` | Rolling major | Latest stable 3.x build |
| `cmake:latest` | Floating | Latest release |

::: warning OCI tags are not immutable
Any tag — including build-tagged ones — can be overwritten by the publisher. Two mechanisms protect your installs regardless of what the registry does later:

- **[Local tag store][fs-tags] snapshot.** The tag → digest mapping only changes when you run `ocx index update`. The snapshot is yours until you decide to refresh it.
- **[Content-addressed package store][fs-packages].** Once a binary is installed, it lives at a path derived from its SHA-256 digest. The tag used to install it is irrelevant — the bytes are permanent.

For absolute reproducibility without relying on any index, reference the digest directly: `cmake@sha256:abc123…`
:::

### Variants {#versioning-variants}

A binary tool can be built multiple ways — with different optimization profiles (`pgo`, `pgo.lto`), feature toggles (`freethreaded`), or size trade-offs (`slim`). Variants describe *how* a binary was built, not *where* it runs. All variants for a given platform execute on the same host; the user chooses the variant, it is not auto-detected.

Without variant support, publishers create separate packages (`python-pgo`, `python-debug`) to represent these build differences, which loses the semantic connection between builds of the same tool.

::: details Why not Docker's tag-suffix convention?
[Docker Hub][docker-images] encodes variants as tag suffixes (`python:3.12-slim`), but this creates a parsing ambiguity with [semver][semver] prereleases — `3.12-alpha` and `3.12-slim` are syntactically indistinguishable. [Concourse CI][concourse-registry]'s registry image resource documents this explicitly, resorting to a heuristic: only `alpha`, `beta`, and `rc` are treated as prereleases, everything else is a variant suffix. ocx avoids this ambiguity entirely with a variant-*prefix* format.
:::

ocx uses a <Tooltip term="variant-prefix format">The variant name comes before the version, separated by a hyphen: `debug-3.12.5`. Because variants start with a letter and versions start with a digit, the boundary is always unambiguous. Variant names match `[a-z][a-z0-9.]*` — lowercase letters, digits, and dots only.</Tooltip> convention instead. The default variant's tags carry no prefix — `python:3.12` resolves the same build as `python:pgo.lto-3.12` when `pgo.lto` is the default.

| Tag | Meaning |
|---|---|
| `python:3.12` | Default variant at version 3.12 |
| `python:debug-3.12` | Debug variant at version 3.12 |
| `python:debug-3` | Latest version of the debug variant in the 3.x series |
| `python:debug` | Latest version of the debug variant |
| `python:latest` | Latest version of the default variant |

Rolling tags cascade within their variant track: `debug-3.12.5` cascades to `debug-3.12` → `debug-3` → `debug`. Variants never cross — publishing a new `debug` build never updates `pgo.lto` or the default variant's tags. See [Cascades][versioning-cascade] for the full cascade model.

For the default variant, cascading also produces unadorned alias tags: publishing a new default-variant build cascades both the prefixed track (if any) and the bare `3.12.5` → `3.12` → `3` → `latest` track.

:::tip Discovery
`ocx index list python --variants` lists the available variant names for a package without downloading any binaries.
:::

<Terminal src="/casts/variants.cast" title="Working with variants" collapsed />

:::warning Platform vs. variant
[OCI `platform.variant`][oci-image-index] is a CPU sub-architecture field (ARM `v6`, `v7`, `v8`) — it determines *where* a binary can run and is selected automatically. OCX software variants determine *how* a binary was built and are selected by the user. The two concepts are orthogonal.
:::

### Cascades {#versioning-cascade}

Publishers are expected to maintain the full tag hierarchy.
When `cmake:3.28.1_20260216120000` is released, all rolling ancestors are re-pointed — but only if this is genuinely the latest at each specificity level:

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

Publishing `cmake:3.27.5_20260217` would update `cmake:3.27` but not `cmake:3` or `cmake:latest` — the `3.28.x` series is still ahead.
Rolling tags only advance, never regress.
Note this is a convention, not a guarantee enforced by the registry — publishers must maintain the cascade manually.

Cascades operate within a single variant track. Publishing `debug-3.28.1` updates `debug-3.28`, `debug-3`, and `debug` — but never touches the default variant's tags (`3.28`, `3`, `latest`) or any other variant's tags. For the default variant, cascading also produces unadorned alias tags that mirror the variant-prefixed chain. See [Variants][versioning-variants] for details.

::: tip Cascading a new release
[`ocx package push --cascade`][cmd-package-push] handles the full cascade automatically: publish one build and let ocx re-point all rolling ancestors in a single command.
:::
::: details Choosing a tag
- **Auditable, specific** → `cmake:3.28.1_20260216120000`
- **Stable with patch fixes** → `cmake:3.28.1` or `cmake:3.28`
- **Follow active development** → `cmake:3` or `cmake:latest`
:::

### Platforms {#versioning-platforms}

The [OCI Image Index specification][oci-image-index] defines a <Tooltip term="multi-platform manifest">An image index is a top-level OCI manifest that lists multiple per-platform sub-manifests under a single tag. Clients resolve the entry that matches their platform; the tag and repository name stay the same across all platforms.</Tooltip> format that allows multiple platform builds to live under a single tag. When you install a package, ocx reads your running system and selects the matching build automatically — no flags, no configuration, no separate repository per architecture.

| System | Detected as |
|---|---|
| Linux on x86-64 | `linux/amd64` |
| Linux on ARM (Graviton, Ampere, RPi 4+) | `linux/arm64` |
| macOS on Intel | `darwin/amd64` |
| macOS on Apple Silicon | `darwin/arm64` |
| Windows on x86-64 | `windows/amd64` |

If a package publishes a universal build with no platform field in the manifest, ocx uses it as a fallback — common for interpreted runtimes and truly portable binaries.

When you need a specific variant for cross-compilation, a Docker image build for a different architecture, or a CI matrix test, pass `-p, --platform`:

::: code-group
```shell [ocx]
ocx install -p linux/arm64 libssl:3
```

```shell [apt equivalent]
# Register the foreign arch, add a ports mirror, then install
sudo dpkg --add-architecture arm64
sudo apt-get update
sudo apt-get install -y libssl-dev:arm64
```
:::

::: details Richer than OS and architecture
The OCI platform model supports finer-grained descriptors that publishers can use for precise compatibility targeting:

- **`variant`** — CPU sub-architecture. For ARM: `v6`, `v7` (32-bit), `v8` (64-bit / arm64). ocx selects the most specific match.
- **`os.version`** — OS version string, primarily used for Windows Server editions (e.g. `10.0.14393.1066`).
- **`os.features`** — OS capability flags, e.g. `win32k` for Windows container isolation modes.
- **`features`** — Reserved for future platform extensions in the OCI spec.

ocx matches all declared fields when selecting among manifest entries. See the [OCI Image Index specification][oci-image-index] for the complete field reference.

Note: OCI `platform.variant` is a CPU sub-architecture field, distinct from OCX [software variants][versioning-variants] which describe build-time characteristics like optimization profiles or feature sets.
:::

### Locking {#versioning-locking}

The most direct lock is a digest: `cmake@sha256:abc123…` bypasses the [tag store][fs-tags] entirely and identifies an exact binary regardless of what any tag points to. Because all installed bytes are [content-addressed][fs-packages], every package can be pinned this way — no lockfiles, no registry queries, just the hash.

For most use cases, the [local tag store snapshot][fs-tags] already provides the lock. Tags resolve to the digest recorded at last update, and that mapping does not change until you run `ocx index update`. A CI runner that never updates its tag store gets the same binary on every run.

For tool authors distributing ocx-powered workflows, there is a more ergonomic option. The [local tag store][fs-tags] holds only metadata — small JSON files, no binaries — so it can be shipped inside a [GitHub Action][github-actions-docs], [Bazel Rule][bazel-rules], or [DevContainer Feature][devcontainer-features]. The result is a two-level lock: the *tool version* locks the *tag store snapshot*, which locks the *resolved binary*. Users pin the tool and get deterministic builds without managing digests, platform conditionals, or separate lockfiles.

The contrast with maintaining a [hand-curated URL matrix][toolchains-llvm] — one `filename → checksum` entry per `version × os × arch` — is clear: a version bump means editing one rule version, not a dictionary.

::: tip GitHub Action with bundled index
```yaml
- uses: ocx-actions/setup-cmake@v2.1.0   # pins action → pins index → pins binary
  with:
    version: "3.28"                       # human-readable tag, no platform conditions
```

`@v2.1.0` locks everything end-to-end. `@v2` follows minor releases — as the maintainer ships updated index snapshots, `cmake:3.28` may resolve to a newer build when the action version changes. No SHA256 lists, no `if: runner.os == 'Linux'` conditionals.
:::

The same pattern applies to [Bazel Rules][bazel-rules] and [DevContainer Features][devcontainer-features-list] — index updates travel as part of the tool release, not as a separate step for end users.

## Indices {#indices}

OCI tags are mutable — `cmake:3` today may point to a different digest after the next patch release. The [versioning section][versioning-tags] covers this in detail. For a tool that installs binaries reproducibly, this is a problem: `ocx install cmake:3` run twice on different days could silently resolve different builds.

ocx solves this by keeping a local snapshot of tag-to-digest mappings. That snapshot only changes when you explicitly refresh it. The same snapshot always resolves `cmake:3` to the same digest — regardless of what the registry serves today.

### Remote Index {#indices-remote}

The remote index is the live OCI registry. It answers metadata queries — which tags exist for a package, which digest a tag currently points to, which platforms a manifest declares — directly and authoritatively. Use [`ocx index catalog`][cmd-index-catalog] to browse available packages or [`ocx index list`][cmd-index-list] to see the tags for a specific package against the live registry with [`--remote`][arg-remote].

The remote index is the data source for [`ocx index update`][cmd-index-update]. Refreshing the local snapshot means querying the remote index and writing the results to disk.

### Local Index {#indices-local}

The local index reads from [`~/.ocx/tags/`][fs-tags] — a <Tooltip term="snapshot">A point-in-time copy of the remote registry's tag-to-digest mappings. No binaries — only the small JSON metadata files needed to resolve package identifiers.</Tooltip> of OCI tag-to-digest mappings. Resolving `cmake:3` is a file read; no network request is made.

**The local index is never updated automatically.** You decide when your snapshot changes. Until you explicitly refresh it, the same identifier always resolves to the same digest — on your laptop, on CI, and on every team member's machine. Rolling tags like `cmake:3` map to the digest current at last update, not whatever the registry serves today.

The snapshot is small enough — JSON metadata only, no binaries — to ship *inside* a [Bazel Rule][bazel-rules] or [GitHub Action][github-actions-docs]. Those tools bundle a frozen snapshot at release time and set [`OCX_INDEX`][env-ocx-index] (or pass [`--index`][arg-index]) to point `ocx` at it; consumers write `cmake:3` and the bundled snapshot resolves it deterministically, with the [package store][fs-packages] and [install symlinks][fs-symlinks] remaining in `OCX_HOME` as usual.

::: tip Out-of-the-Box Support for Dependabot and Renovate
A single version bump to the action or rule — proposed automatically by [Dependabot][dependabot] or [Renovate][renovate] — advances the bundled index. Users get the updated binary with no config changes. No [per-platform URL matrix][toolchains-llvm] to hand-edit, no separate PR to bump the tool itself.
:::

[`ocx index update <package>`][cmd-index-update] syncs the local index for a specific package from the remote registry. A bare identifier (e.g., `cmake`) downloads all tag-to-digest mappings; a tagged identifier (e.g., `cmake:3.28`) fetches only that single tag's digest and manifest, merging it into the existing local tags file. This tag-scoped mode is ideal for lockfile workflows where the index should contain only explicitly requested tags. Packages not listed are not touched.

*Commands: [`ocx index update`][cmd-index-update], [`ocx index catalog`][cmd-index-catalog], [`ocx index list`][cmd-index-list]*

### Active Index {#indices-selected}

Every command that resolves a package identifier — [`ocx install`][cmd-install], [`ocx find`][cmd-find], [`ocx exec`][cmd-exec], [`ocx index list`][cmd-index-list] — uses one working index for that invocation. By default, this is the local index. Two flags change which index is used:

| Mode | Flag | Source | Network? |
|---|---|---|---|
| Default | *(none)* | Local snapshot | No (unless fetching a new binary) |
| Remote | [`--remote`][arg-remote] | OCI registry | Yes |
| Offline | [`--offline`][arg-offline] | Local snapshot | Never |

**`--remote`** routes tag and catalog lookups to the registry directly for a single command. Pure-query commands ([`ocx index list`][cmd-index-list], [`ocx index catalog`][cmd-index-catalog], [`ocx package info`][cmd-package-info]) do **not** persist the result to the local index — to refresh the snapshot, run [`ocx index update`][cmd-index-update] explicitly. Use `--remote` for a one-off check (current available tags, latest digest) without committing the resolution to the local snapshot.

**`--offline`** prevents all network access for that command. If the local index does not have a requested package, the command fails immediately rather than attempting a registry query. Useful to verify that your current index and object store are self-sufficient before a build in a restricted or air-gapped environment.

[`--index`][arg-index] / [`OCX_INDEX`][env-ocx-index] do not change the active index mode — the local snapshot remains active. They only change *where* that snapshot is read from. See [Local Index](#indices-local).

The active index controls tag and manifest resolution only. The [package store][fs-packages] is independent — installed binaries are accessible in all three modes regardless of which index is active.

### Routing {#indices-routing}

Behind the user-facing flags, ocx encodes two orthogonal axes at every index call site:

- **Routing axis** — *where* a lookup reads from, controlled by the active mode plus the immutability of the identifier (digest-addressed reads always trust the local index first; tag-addressed reads route per mode).
- **Intent axis** — *whether* a lookup may mutate the local index. Pure-query commands (`index list`, `index catalog`, `package info`) declare `Query` intent and never trigger a write; install/pull commands declare `Resolve` intent and may persist.

The routing matrix below summarises the combinations:

| Operation | `--remote` | `--offline` | `--offline --remote` | Default |
|-----------|-----------|-------------|----------------------|---------|
| Tag list / catalog (pure query) | source only, no write | local only | local only (info log) | local only |
| Tag → manifest, install/pull | source only, write blobs+tag | local only (errors if missing) | local only (errors) | local first, miss → fetch+write |
| Digest → manifest, any path | local first | local only | local only | local first |
| Digest → manifest, pinned-id pull | source on miss, write blobs only, **no tag** | local only | local only | local first, miss → fetch blobs only |

The "no tag" cell on the last row is the post-pin contract: when `ocx.lock` already pins a tool to a digest, `ocx pull` persists the manifest blobs but does not commit a tag pointer. The lock is the canonical record of the tag→digest mapping; writing through would silently shadow it.

::: info Why this matters
Pre-Phase-11, a pure `ocx --remote index list cmake` would silently write through to `$OCX_HOME/tags/` on cache miss — the trait conflated "look this up" with "look this up and persist". Build-pipeline workarounds existed only to mask this leak. With intent declared at every call site, queries are guaranteed read-only.
:::

### Pinned-only mode {#indices-pinned-only}

Combining [`--offline`][arg-offline] with [`--remote`][arg-remote] is accepted as **pinned-only mode**: no source contact, no local writes, and any tag-addressed resolution that cannot be satisfied locally errors instead of silently falling back. The CLI emits an `info` log to confirm the mode is active.

Use it to assert in CI that every project dependency is digest-pinned:

```sh
ocx --offline --remote exec -- my-build-script
```

If any tool resolution falls back to a floating tag, the command fails — a hermetic-build sanity check without round-tripping to the registry.

## Authentication {#authentication}

ocx utilizes a layered approach of different authentication methods.
Most of these are specific to the registry being accessed, allowing to configure different authentication methods for different registries.
The following authentication methods are supported and queried in the the order they are listed.
If one of the methods is successful, the rest will be ignored.

- [Environment variables][auth-env-vars]
- [Docker credentials][auth-docker-creds]

### Environment Variables {#authentication-environment-variables}

To configure authentication for a registry you can use environment variables.
The following examples show how to configure authentication for the registry `docker.io` using a bearer token or basic authentication.

::: code-group
```sh [bearer]
export OCX_AUTH_docker_io_TYPE=bearer
export OCX_AUTH_docker_io_TOKEN="<token>"
```

```sh [basic]
export OCX_AUTH_docker_io_TYPE=basic
export OCX_AUTH_docker_io_USER="<user>"
export OCX_AUTH_docker_io_TOKEN="<token>"
```
:::

The above examples configures the following environment variables:

- [`OCX_AUTH_<REGISTRY>_TYPE`][env-auth-type]: The type of authentication to use for the registry.
- [`OCX_AUTH_<REGISTRY>_USER`][env-auth-user]: The username to use for authentication.
- [`OCX_AUTH_<REGISTRY>_TOKEN`][env-auth-token]: The password to use for authentication.

::: info
The registry name in the variable is normalized by replacing all non-alphanumeric characters with underscores.
For example, for the registry `docker.io`, ocx will look for `OCX_AUTH_docker_io_TYPE`.
This is stricter than the [path component encoding](#file-structure) used for filesystem paths, which preserves dots and hyphens.
:::

The user and token are used according to the authentication type specified.
If no authentication type is specified but a user or token is configured, ocx will infer the authentication type.
For example, if a user and token are configured, ocx will assume basic authentication.
If only a token is configured, ocx will assume bearer authentication.
For more information see the documentation of the [environment variables][env-auth-type].

### Docker Credentials {#authentication-docker-credentials}

If a docker configuration is found, ocx will attempt to use the stored credentials for authentication.
The configuration is typically stored in the file `~/.docker/config.json` and can be configured using the `docker login` command.

```sh
docker login "<registry>"
```

The docker configuration file location can be overridden by setting the [`DOCKER_CONFIG`][env-docker-config] environment variable.

## Dependencies {#dependencies}

Package publishers can declare that their package requires other packages to function. A web application might need a JavaScript runtime; a build tool might need a compiler. When a publisher builds their package, they pin each dependency to an exact <Tooltip term="OCI digest">A SHA-256 fingerprint that identifies a specific build of a package. The publisher records the exact digest they tested against — not a version range, not a "latest" tag. This means the dependency graph is fully determined by the package metadata alone.</Tooltip>, recording the precise build they tested against.

As a user, you do not need to manage these dependencies yourself. When you install or run a package, ocx handles them automatically.

### Resolution {#dependencies-resolution}

When you pull or install a package that declares dependencies, ocx fetches every required package transitively — dependencies of dependencies, and so on — and stores them in the [package store][fs-packages]. The process is fully automatic and requires no extra flags:

```shell
ocx package pull webapp:2.0
```

If `webapp:2.0` declares dependencies on `nodejs:24` and `bun:1.3`, all three packages end up in the package store. Only `webapp:2.0` is the package you explicitly requested — the dependencies are implementation details, fetched and stored but not surfaced as top-level installs.

To actually *run* the package with its dependency environments configured, use [`ocx exec`][cmd-exec]:

```shell
ocx exec webapp:2.0 -- serve --port 8080
```

[`ocx exec`][cmd-exec] composes the environments of all dependencies in the correct order before launching the command. Running a package binary directly — without `ocx exec` — does not set up dependency environments. There are no launcher scripts yet; environment composition always goes through [`ocx exec`][cmd-exec] or [`ocx env`][cmd-env].

::: warning install + select does not set up dependency environments
[`ocx install --select`][cmd-install] creates a [current symlink][fs-symlinks] that points to the package's content directory. If you or another tool invokes a binary through that symlink directly, the dependency environments are **not** configured — only the package's own files are accessible. For packages with dependencies, always use [`ocx exec`][cmd-exec] to run commands, or [`ocx env`][cmd-env] / [`ocx shell env`][cmd-shell-env] to export the full environment into your shell first.
:::

::: tip You can still install dependencies explicitly
If you want `nodejs:24` available as a standalone tool alongside its role as a dependency, install it separately:

```shell
ocx install --select nodejs:24
```

Both references — the dependency relationship and your explicit install — coexist. The binary is stored once in the package store (content-addressed), and both references protect it from [garbage collection][cmd-clean].
:::

### Environment {#dependencies-environment}

When you run [`ocx exec`][cmd-exec] or [`ocx env`][cmd-env] with a package that has dependencies, ocx builds the environment by applying each package's declared variables in <Tooltip term="topological order">Dependencies are applied before their dependents. If A depends on B, and B depends on C, the order is C → B → A. Among packages at the same level, alphabetical order (by identifier) is used as a tiebreaker so the result is deterministic.</Tooltip>. Dependencies come first, then the package you requested.

This means a package can override a dependency's scalar variables (like `JAVA_HOME`) while accumulator variables (like `PATH`) are merged naturally — each dependency's `bin/` directory is prepended in order.

::: warning Conflicting scalar variables
If two dependencies set the same scalar variable (e.g., both set `JAVA_HOME` to different paths), ocx applies <Tooltip term="last-writer-wins">The last package in topological order that sets a scalar variable determines the final value. This is the same behavior as listing multiple packages in `ocx exec pkg1 pkg2 -- cmd` today.</Tooltip> semantics and emits a warning. This is the same behavior as listing multiple packages manually in [`ocx exec`][cmd-exec]. If you see a conflict warning, inspect the dependency tree with [`ocx deps --flat`][cmd-deps] to understand the evaluation order.

This is especially common with the [shell profile][cmd-shell-profile]. If you add a package with dependencies to your profile (e.g., `webapp:2.0` which depends on `nodejs:24`) and also have `nodejs:24` in your profile as a standalone tool, both will contribute environment variables. The profile loads all packages via [`ocx shell env`][cmd-shell-env], which includes dependency environments — so `NODE_HOME` may be set twice from different content paths. To avoid this, either remove the standalone entry from the profile (the dependency provides it), or accept that the profile entry's value wins (it appears later in the load order).
:::

### Visibility {#dependencies-visibility}

Not every dependency's environment should leak to consumers. A build tool that wraps a compiler
might need `CC` and `LD_LIBRARY_PATH` internally, but users of the build tool shouldn't see those
variables in their own environment. Conversely, a meta-package that bundles a runtime stack
should forward everything to whoever depends on it.

The `visibility` field on each dependency entry controls this. It has four levels:

| Level | Self | Export | Example |
|---|---|---|---|
| `sealed` *(default)* | No | No | Structural dep — accessed by path, not env. |
| `private` | Yes | No | Internal tool needed for own shims/entry points. |
| `public` | Yes | Yes | Shared runtime both sides need (e.g. Java). |
| `interface` | No | Yes | Meta-package that composes env for consumers. |

```json
{
  "dependencies": [
    { "identifier": "ocx.sh/java:21", "digest": "sha256:a1b2…", "visibility": "private" },
    { "identifier": "ocx.sh/maven:3",  "digest": "sha256:c3d4…", "visibility": "public" }
  ]
}
```

In this example, `java` is private — the package needs it internally but consumers don't get
`JAVA_HOME`. `maven` is public — both the package and its consumers see Maven's environment.

When dependencies form chains, visibility propagates using a simple rule: **if the child exports
(public or interface), the result equals the parent's edge; otherwise sealed.** When two paths
reach the same dependency through a diamond, the most open visibility wins.

The tree view annotates non-public dependencies so you can see visibility at a glance — `(private)`
deps are used internally, `(sealed)` deps are structural only, and `public` deps have no annotation:

<Terminal src="/casts/deps.cast" title="Dependency tree with visibility" collapsed />

The flat view shows the effective visibility in its own column — this is the primary tool for
debugging which dependencies actually contribute to the environment:

<Terminal src="/casts/deps-flat.cast" title="Flat view with visibility column" collapsed />

::: details Self-execution path
All four visibility levels are fully implemented in the propagation algebra and stored in
`resolve.json`. The consumer-visible axis (`public`, `interface`) controls which deps contribute
to [`ocx exec`][cmd-exec] and [`ocx env`][cmd-env] today. The self-visible axis (`public`,
`private`) will activate when self-execution environments (shims, entry points) are added in
a future release — at that point, `private` deps will contribute to a package's own shim
execution but remain invisible to consumers.
:::

### Inspection {#dependencies-inspection}

[`ocx deps`][cmd-deps] shows the dependency tree for installed packages. The default view is a logical tree showing the declared relationships:

<Terminal src="/casts/deps.cast" title="Inspecting the dependency tree" collapsed />

Use `--flat` to see the resolved evaluation order — the exact sequence ocx uses when composing environments. This is the primary debugging tool when environment variables are not what you expect:

<Terminal src="/casts/deps-flat.cast" title="Resolved dependency order" collapsed />

When a transitive dependency causes an unexpected conflict, `--why` traces the path from a root package to the dependency in question:

<Terminal src="/casts/deps-why.cast" title="Tracing why a dependency is pulled in" collapsed />

### Cleanup {#dependencies-gc}

Dependencies are protected from garbage collection as long as any package that depends on them is still referenced. When you uninstall a package, its dependencies do not disappear immediately — they remain in the package store until no other installed package depends on them. [`ocx clean`][cmd-clean] removes only packages that have no references at all: no install symlinks and no dependent packages.

::: details How dependency protection works
When ocx installs a package with dependencies, it records each dependency as a <Tooltip term="forward-ref">A symlink stored inside the dependent package's own `refs/deps/` directory that points at the dependency's `content/`. The dependent, not the dependency, owns the reference — so the link points forward along the dependency edge.</Tooltip> inside the dependent package's `refs/deps/` directory, pointing at the dependency's `content/`. Nothing is written into the dependency's own `refs/symlinks/`. [`ocx clean`][cmd-clean] keeps a dependency alive as long as any reachable package has a matching `refs/deps/` entry — the dependency is protected by the liveness of its dependents, not by a back-reference inside itself.

[`ocx clean`][cmd-clean] processes the full dependency graph in a single pass: it walks `refs/deps/`, `refs/layers/`, and `refs/blobs/` forward-refs from each root, marks everything reachable, and deletes what remains. Any dependency that loses its last dependent in the same sweep becomes unreachable and is collected alongside it. No manual intervention is needed.
:::

### Scope {#dependencies-scope}

ocx dependencies are deliberately simple — they solve a specific problem without introducing the complexity of a full-featured dependency resolver.

**No version ranges.** A dependency is pinned to an exact digest. There is no "find me any Java >= 21" logic. The publisher chose a specific build, tested against it, and recorded it. This is what makes the dependency graph fully reproducible: the metadata is the complete truth, regardless of what the registry contains today.

**No automatic updates.** When a dependency gets a security patch, the publisher must release a new version of their package with the updated digest. This is a deliberate tradeoff — reproducibility over convenience. Future tooling will help publishers detect when their pinned dependencies have newer builds available.

**Not a project-level lockfile.** Dependencies live in package metadata. They describe what a *single package* needs, not what a *project* needs. A project-level configuration file with version resolution and lock semantics is a separate concept for a future release.

::: info How other tools compare
[Nix][nix] takes the same approach: every derivation pins its inputs by hash, and the store is content-addressed. There are no version ranges — the exact input is determined at build time. [Go modules][go-modules] use Minimum Version Selection with a `go.sum` integrity file — deterministic, but with a resolution algorithm. [Homebrew][homebrew] uses floating name-only dependencies with no pinning at all.

ocx sits closest to Nix in philosophy — exact pins, no resolution — but without the functional language and the build system. The dependency declaration is a flat list of digests in a JSON file.
:::

## Configuration {#configuration}

OCX behavior is controlled at three layers: config files, environment variables, and CLI flags. Higher layers always win — CLI flags override environment variables, which override config files.

Config files are in [TOML][toml] format and live in three locations:

| Tier | Path |
|------|------|
| System | `/etc/ocx/config.toml` |
| User (Linux) | [`$XDG_CONFIG_HOME`][xdg-basedir]`/ocx/config.toml` or `~/.config/ocx/config.toml` |
| User (macOS) | `~/Library/Application Support/ocx/config.toml` (`XDG_CONFIG_HOME` is not consulted on macOS) |
| OCX home | [`$OCX_HOME`][env-ocx-home]`/config.toml` (default: `~/.ocx/config.toml`) |

Files are loaded lowest-to-highest and merged. Missing files are silently skipped. No config file is required.

**Explicit additions.** [`--config`][arg-config] `FILE` or [`OCX_CONFIG_FILE`][env-config-file]`=/path/to/file.toml` layers an extra file on top of the discovered chain — useful for refining ambient config without rewriting it. Both can be set together ([`--config`][arg-config] sits at highest file-tier precedence). The specified file must exist. To disable an ambient [`OCX_CONFIG_FILE`][env-config-file] without unsetting it, set it to the empty string.

**Kill switch.** [`OCX_NO_CONFIG`][env-no-config]`=1` skips the discovered chain (system, user, [`$OCX_HOME`][env-ocx-home]) but leaves explicit paths intact. Combine with [`--config`][arg-config] or [`OCX_CONFIG_FILE`][env-config-file] for a fully hermetic CI load: `OCX_NO_CONFIG=1 ocx --config ci.toml ...`.

See the [Configuration reference][config-ref] for the full list of config keys, merge rules, examples, and error messages.

## CI Integration {#ci}

CI environments need tool binaries to be available and their environment variables exported — but
they don't need version switching, candidate symlinks, or any of the install-store machinery that
supports interactive use. OCX provides two commands tailored for this:

[`package pull`][cmd-package-pull] downloads packages into the content-addressed
[package store][fs-packages] without creating any symlinks. [`ci export`][cmd-ci-export] then writes
the package-declared environment variables directly into the CI system's runtime files.

```shell
ocx package pull cmake:3.28
ocx ci export cmake:3.28
```

On [GitHub Actions][github-actions-docs], `ci export` auto-detects the environment and appends
`PATH` entries to `$GITHUB_PATH` and other variables to `$GITHUB_ENV`, making them available in
all subsequent steps.

:::tip
`package pull` only touches the package store — no symlinks, no symlink-store mutations. This makes
it safe to run concurrently in matrix builds that share a cached [`OCX_HOME`][env-ocx-home], since
content-addressed writes are inherently idempotent.
:::

:::info Relationship to `install`
[`install`][cmd-install] is `package pull` plus candidate-symlink creation (and optionally
`--select` for the current symlink). In CI you typically don't need symlinks — the
content-addressed package-store path that `package pull` reports is fully reproducible and
digest-derived.
:::

## Project Toolchain {#project-toolchain}

A project-tier `ocx.toml` declares the tools a repository depends on — every contributor and CI runner resolves to identical, digest-pinned binaries without arguing about versions over chat. The shell profile mechanism that ships in earlier OCX releases ([`shell profile add`][cmd-shell-profile]) is interactive and per-user; it cannot describe what a repository expects, only what an individual machine has installed.

The project toolchain closes that gap. A committed `ocx.toml` plus its sibling `ocx.lock` makes "the tools this project needs" a piece of source code: reviewable, mergeable, reproducible across machines, and resolvable offline once the lock is fetched.

### Declaring tools — `ocx.toml` {#project-toolchain-toml}

`ocx.toml` lives at the root of a repository (or anywhere up the directory tree from `cwd`). It is a [TOML][toml] file with a single `[tools]` table mapping local binding names to fully-qualified [OCI identifiers][oci-identifier]:

```toml
[tools]
cmake    = "ocx.sh/cmake:3.28"
ripgrep  = "ocx.sh/ripgrep:14"
mytool   = "ghcr.io/acme/mytool:1.0"
```

:::info
If you use [mise][mise] or [asdf][asdf], `ocx.toml` fills a different role: mise/asdf manage what is installed on a developer's workstation; `ocx.toml` + `ocx.lock` pin cryptographically what a repository requires — including private OCI registry tools that mise/asdf have no plugin for.
:::

Each value is `registry/repo:tag` (optionally `@digest`); bare-tag forms like `cmake = "3.28"` are rejected at parse time so an `ocx.toml` is unambiguous regardless of which registry the user has configured as their default. The binding name on the left is independent of the repository path — `mytool = "ghcr.io/acme/mytool:1.0"` is fine and lets internal projects rename tools without touching the registry.

The schema is published at [`https://ocx.sh/schemas/project/v1.json`][schema-project] and wired through [taplo][taplo] for editor auto-completion. With `taplo` installed and an `ocx.toml` open in [Helix][helix], [VSCode][vscode-taplo], or [Neovim][neovim-taplo-lsp], unknown fields surface as red squiggles and tool names are completed as you type.

:::warning Avoid global state in `ocx.toml`
The project file describes tools the project needs, not how a contributor's shell prompt should behave. Shell-profile state (`PATH` munging, sentinel env vars, "load on every prompt") stays in the user-tier mechanisms — see [interactive activation][project-toolchain-activation] for how the project tier hooks into a developer's shell without spilling into `ocx.toml`.
:::

### Locking — `ocx.lock` {#project-toolchain-lock}

`ocx.toml` declares advisory tags (`cmake:3.28`); the registry resolves those tags to immutable digests. To make the project reproducible, [`ocx lock`][cmd-lock] resolves every tag once and writes the result to `ocx.lock` next to `ocx.toml`. Subsequent commands read the lock, never the registry, so two machines running `ocx pull` from the same commit get the same bytes.

The lock carries a `declaration_hash` over the canonicalized `ocx.toml` ([RFC 8785 JCS][rfc-8785]). When you change `ocx.toml`, the hash changes; commands that depend on the lock ([`ocx pull`][cmd-pull], [`ocx exec`][cmd-exec]) detect the mismatch and refuse to run with stale digests. Re-run `ocx lock` to regenerate the file.

:::warning Commit your `ocx.lock`
`ocx.lock` is what makes the project reproducible — without it, every contributor and CI runner re-resolves advisory tags against whatever the registry surfaces today. Commit it alongside `ocx.toml`.

To keep merge conflicts manageable on busy projects, add a [`.gitattributes`][gitattributes] entry that lets `git` union sibling lock entries instead of choking on overlapping diff hunks:

```text
ocx.lock merge=union
```

The `merge=union` driver concatenates both sides of a conflict; each `[[tool]]` entry is independent, so the result is a syntactically valid lock file in most cases — duplicate `[[tool]]` entries are concatenated; running `ocx lock` after the merge normalizes the result and resolves any sort-order or deduplication concerns.
:::

:::warning `ocx.lock` is machine-generated
Do not hand-edit `ocx.lock`. The format is canonicalized — sort order, whitespace, and the per-entry `declaration_hash` are computed by `ocx lock` and may evolve across OCX versions. Manual edits will be overwritten on the next `ocx lock` run and may be rejected by future schema versions. The schema's top-level `$comment` reinforces this for tooling consumers; treat it as read-only source code.

Tooling that reads `ocx.lock` should validate against the published schema at [`https://ocx.sh/schemas/project-lock/v1.json`][schema-lock]; the schema's `$comment` field carries a machine-readable do-not-edit marker recognizable to JSON Schema processors.
:::

### Pulling and executing {#project-toolchain-pull-exec}

Once `ocx.lock` exists, two commands cover the bulk of day-to-day use. [`ocx pull`][cmd-pull] pre-warms the [package store][fs-packages] from the lock without creating install symlinks — ideal for CI matrix builds and developer machines that already have a [direnv][direnv] hook in place. [`ocx exec`][cmd-exec] runs a command with the project's resolved environment, treating the lock as the source of truth:

```shell
ocx pull
ocx exec cmake -- cmake --version    # uses cmake from ocx.lock, not the system
```

`ocx exec` gates on the same `declaration_hash` check as `ocx pull`: if the lock is stale, the command exits with a structured error pointing at `ocx lock`. There is no implicit re-resolution — the project file is the input, the lock file is the contract, and registry round-trips happen only when you ask for them.

### Groups {#project-toolchain-groups}

Not every contributor needs every tool. CI needs `shellcheck` and `shfmt`; the release pipeline needs `goreleaser`; daily development needs neither. Named groups let `ocx.toml` describe these subsets without forcing every workstation to download release tooling on first checkout:

```toml
[tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci]
shellcheck = "ocx.sh/shellcheck:0.10"
shfmt      = "ocx.sh/shfmt:3.7"

[group.release]
goreleaser = "ocx.sh/goreleaser:2.0"
```

The top-level `[tools]` table is the implicit `default` group; named `[group.<name>]` tables add to it. `[group.default]` is reserved and produces a parse error — there is no ambiguity between "implicit default" and "named default."

Pass `--group` (repeatable, comma-separated) to scope a command:

```shell
ocx pull -g ci,lint               # CI runner — only what's needed for lint jobs
ocx pull -g release               # release runner — only release tools
ocx lock                          # workstation — every group resolved
```

When a tool name appears in both `[tools]` and a `[group.*]` table, the parser rejects the file with a structured error naming the duplicate. The same name in two different groups is allowed — it's the resolver's job to surface conflicts at exec time, not the schema's.

### Interactive shell activation {#project-toolchain-activation}

A project's tools should be on `PATH` whenever you `cd` into the project — without `eval`-ing anything on every shell startup. OCX ships three ways to wire that, each suited to a different workflow.

The hooks only export variables — they never install missing tools, never contact the registry, and never mutate the [package store][fs-packages]. Use [`ocx pull`][cmd-pull] to materialize anything `ocx.lock` requires before activation.

[`ocx hook-env`][cmd-hook-env] is the prompt-hook entry point. It reads the nearest `ocx.toml` plus its lock, computes a fingerprint over the actually-installed tools, and emits export lines only when something changed. The fingerprint lives in the `_OCX_APPLIED` environment variable; if `ocx hook-env` is invoked twice with the same lock and the same on-disk tool set, the second invocation prints nothing and exits zero. This is how the prompt stays cheap.

[`ocx shell-hook`][cmd-shell-hook] is the [direnv][direnv] entry point. It is stateless — no `_OCX_APPLIED`, no diffing — and emits a fresh export block on every invocation. `direnv` supplies the cache layer (one re-evaluation per `cd`, watched files re-trigger), so the hook stays simple. Run [`ocx generate direnv`][cmd-generate-direnv] in a project directory to drop a ready-made `.envrc`, then `direnv allow`.

[`ocx shell init <shell>`][cmd-shell-init] prints a one-time snippet you append to `~/.bashrc`, `~/.zshrc`, or your fish/nushell config. The snippet wires `ocx hook-env` into the shell's prompt-hook mechanism (`PROMPT_COMMAND` for Bash, `precmd` for Zsh, `fish_prompt` for Fish, `pre_prompt` for Nushell) so every prompt re-evaluates the project toolchain. All three commands work with both the project-tier `ocx.toml` and the [home-tier `ocx.toml`][project-toolchain-home-tier] — the resolver decides which file is in scope, the hooks emit exports for whichever wins.

:::tip Choose one entry point per workflow
The three hooks are not meant to coexist in the same shell. `direnv` users want `ocx shell-hook`; pure shell-builtin users want `ocx shell init`; tooling that needs to skip the prompt-hook ceremony (CI, scripted environments) calls `ocx exec` directly with no hook at all.
:::

### Home-tier `ocx.toml` {#project-toolchain-home-tier}

A user-wide `ocx.toml` lives at `$OCX_HOME/ocx.toml` (default `~/.ocx/ocx.toml`) and serves as a fallback when a developer's CWD has no project file in sight. Tools declared here become the implicit project — handy for scratch directories, system shells, and one-off invocations where you still want `ripgrep` or `cmake` available.

The home-tier file uses the same [schema][schema-project] and the same lock semantics; the lock lives at `$OCX_HOME/ocx.lock`. Commands that consult the project tier walk the directory tree first; only when the walk finds nothing does the home file activate.

:::warning Project replaces home — never merges
Per [ADR Amendment C][adr-project-toolchain-amendment-c], a project-tier `ocx.toml` and a home-tier `ocx.toml` never compose. Whichever the resolver finds first is the only one that contributes. This avoids the `cargo`-style "config-merging" cascade where it's hard to predict which file declared which tool.
:::

To opt out entirely (one-off CI invocation, hermetic test), set `OCX_NO_PROJECT=1`. The home-tier fallback is suppressed even when `$OCX_HOME/ocx.toml` exists.

### Migration from `shell profile` {#project-toolchain-migration}

The earlier shell-profile mechanism — [`ocx shell profile add`][cmd-shell-profile-add], `remove`, `list`, `load` — is deprecated in v1 and will be removed in v2. The project toolchain replaces it with a file-based, per-repo declaration that survives across machines and review cycles.

In v1 (this release), the `shell profile add`, `remove`, `list`, and `load` subcommands continue to work but emit a one-line deprecation note on stderr pointing at this guide. Existing users have a release cycle to migrate; nothing breaks today.

In v2 (next breaking release), `shell profile add`, `shell profile remove`, `shell profile list`, and `shell profile load` are removed. The recommended migration path is:

- For project-scoped tools, declare them in `ocx.toml` and pick an [activation hook][project-toolchain-activation] — `ocx shell init <shell>` is the closest one-line replacement for `shell profile load`.
- For user-scoped tools that should be on every shell regardless of `cwd`, declare them in `$OCX_HOME/ocx.toml` (the [home-tier file][project-toolchain-home-tier]) and let the same hook trio surface them.

[`ocx shell profile generate`][cmd-shell-profile-generate] survives v2 as a convenience for users who want a one-shot exported init file (sourced once from `~/.bashrc`) without a prompt hook. It is the only `shell profile` subcommand that remains.

### Reproducibility and SLSA {#project-toolchain-reproducibility}

OCX v1 ships digest-pinning reproducibility: every tool a project resolves is identified by its OCI manifest digest, and the lock file commits that digest under a hash of the source `ocx.toml`. Any consumer with the lock can verify they are pulling exactly the bytes the project committed to — no tag races, no silent registry rewrites.

What is not yet shipped is a signed build attestation describing how each tool was produced. That capability — the kind of [SLSA build provenance][slsa-l1] producers can generate via [Sigstore][sigstore] or similar — is deferred to v2. Treat OCX v1 as solid input integrity, not as compliance with any [SLSA level][slsa-attestation].

In practice, the v1 contract is sufficient for the most common reproducibility needs: locking a CI matrix to known-good binaries, surviving registry mutability incidents, and ensuring contributors review tool upgrades the same way they review code changes. v2 will add the cryptographic chain that links published digests to verifiable build pipelines.

<!-- external -->
[toml]: https://toml.io/
[concourse-registry]: https://github.com/concourse/registry-image-resource
[nix]: https://nixos.org/
[go-modules]: https://go.dev/ref/mod
[sdkman]: https://sdkman.io/
[homebrew]: https://brew.sh/
[docker-images]: https://hub.docker.com/search?image_filter=official
[semver]: https://semver.org/
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
[oci-identifier]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-manifests
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[bazel-rules]: https://bazel.build/extending/rules
[devcontainer-features]: https://containers.dev/implementors/features/
[devcontainer-features-list]: https://containers.dev/features
[toolchains-llvm]: https://github.com/bazel-contrib/toolchains_llvm/blob/master/toolchain/internal/llvm_distributions.bzl
[dependabot]: https://docs.github.com/en/code-security/dependabot/working-with-dependabot/keeping-your-actions-up-to-date-with-dependabot
[renovate]: https://docs.renovatebot.com/
[terraform-lockfile]: https://developer.hashicorp.com/terraform/language/files/dependency-lock
[docker-credential]: https://github.com/keirlawson/docker_credential
[helm-oci]: https://helm.sh/docs/topics/registries/
[docker-layers]: https://docs.docker.com/get-started/docker-concepts/building-images/understanding-image-layers/
[pnpm]: https://pnpm.io/motivation#saving-disk-space
[taplo]: https://taplo.tamasfe.dev/
[helix]: https://helix-editor.com/
[vscode-taplo]: https://marketplace.visualstudio.com/items?itemName=tamasfe.even-better-toml
[neovim-taplo-lsp]: https://github.com/neovim/nvim-lspconfig/blob/master/lua/lspconfig/configs/taplo.lua
[direnv]: https://direnv.net/
[gitattributes]: https://git-scm.com/docs/gitattributes
[rfc-8785]: https://www.rfc-editor.org/rfc/rfc8785
[slsa-l1]: https://slsa.dev/spec/v1.0/levels#build-l1
[slsa-attestation]: https://slsa.dev/spec/v1.0/attestation-model
[sigstore]: https://www.sigstore.dev/
[schema-project]: https://ocx.sh/schemas/project/v1.json
[adr-project-toolchain-amendment-c]: https://github.com/ocxbase/ocx/blob/main/.claude/artifacts/adr_project_level_toolchain_config.md
[mise]: https://mise.jdx.dev/
[asdf]: https://asdf-vm.com/
[schema-lock]: https://ocx.sh/schemas/project-lock/v1.json

<!-- commands -->
[arg-config]: ./reference/command-line.md#arg-config
[cmd-clean]: ./reference/command-line.md#clean
[cmd-package-create]: ./reference/command-line.md#package-create
[cmd-deselect]: ./reference/command-line.md#deselect
[cmd-find]: ./reference/command-line.md#find
[cmd-exec]: ./reference/command-line.md#exec
[cmd-install]: ./reference/command-line.md#install
[cmd-select]: ./reference/command-line.md#select
[cmd-uninstall]: ./reference/command-line.md#uninstall
[cmd-index-catalog]: ./reference/command-line.md#index-catalog
[cmd-index-list]: ./reference/command-line.md#index-list
[cmd-index-update]: ./reference/command-line.md#index-update
[cmd-package-info]: ./reference/command-line.md#package-info
[cmd-package-pull]: ./reference/command-line.md#package-pull
[cmd-package-push]: ./reference/command-line.md#package-push
[cmd-ci-export]: ./reference/command-line.md#ci-export
[cmd-deps]: ./reference/command-line.md#deps
[cmd-env]: ./reference/command-line.md#env
[cmd-shell-env]: ./reference/command-line.md#shell-env
[cmd-shell-profile]: ./reference/command-line.md#shell-profile
[cmd-shell-profile-add]: ./reference/command-line.md#shell-profile-add
[cmd-shell-profile-generate]: ./reference/command-line.md#shell-profile-generate
[cmd-lock]: ./reference/command-line.md#lock
[cmd-pull]: ./reference/command-line.md#pull
[cmd-hook-env]: ./reference/command-line.md#hook-env
[cmd-shell-hook]: ./reference/command-line.md#shell-hook
[cmd-shell-init]: ./reference/command-line.md#shell-init
[cmd-generate-direnv]: ./reference/command-line.md#generate-direnv
[arg-remote]: ./reference/command-line.md#arg-remote
[arg-offline]: ./reference/command-line.md#arg-offline
[arg-index]: ./reference/command-line.md#arg-index

<!-- environment -->
[env-ocx-home]: ./reference/environment.md#ocx-home
[env-ocx-index]: ./reference/environment.md#ocx-index
[env-config-file]: ./reference/environment.md#ocx-config-file
[env-no-config]: ./reference/environment.md#ocx-no-config
[env-auth-type]: ./reference/environment.md#ocx-auth-registry-type
[env-auth-user]: ./reference/environment.md#ocx-auth-registry-user
[env-auth-token]: ./reference/environment.md#ocx-auth-registry-token
[env-docker-config]: ./reference/environment.md#external-docker-config
[xdg-basedir]: ./reference/environment.md#external-xdg-config-home

<!-- reference pages -->
[config-ref]: ./reference/configuration.md

<!-- internal -->
[versioning-variants]: #versioning-variants
[versioning-cascade]: #versioning-cascade
[fs-tags]: #file-structure-tags
[fs-symlinks]: #file-structure-symlinks
[fs-packages]: #file-structure-packages
[versioning-tags]: #versioning-tags
[auth-env-vars]: #authentication-environment-variables
[auth-docker-creds]: #authentication-docker-credentials
[project-toolchain-activation]: #project-toolchain-activation
[project-toolchain-home-tier]: #project-toolchain-home-tier
[faq-build-separator]: ./faq.md#versioning-build-separator
