---
outline: deep
---
# User Guide

## File Structure {#file-structure}

Most package managers store everything in a single mutable tree — installing a new version silently replaces the old one, breaking anything that referenced the old path. They also hit the network on every operation, making offline use and reproducible CI awkward.

### Storage

ocx separates concerns into three independent stores under `~/.ocx/` (configurable via [`OCX_HOME`][env-ocx-home]):

<Tree>
  <Node name="~/.ocx/" icon="🏠" open>
    <Node name="objects/" icon="📦">
      <Description>immutable, content-addressed binary store</Description>
    </Node>
    <Node name="index/" icon="🗂️">
      <Description>local mirror of registry metadata — no binaries</Description>
    </Node>
    <Node name="installs/" icon="🔀">
      <Description>stable symlinks safe to embed in shell profiles and configs</Description>
    </Node>
  </Node>
</Tree>

Each store has a single responsibility. Upgrading a package updates symlinks in `installs/` and may add a new object to `objects/`, but never touches `index/`. The stores can be understood — and reasoned about — one at a time.

::: details Path component encoding
Registry names, repository names, and tags that appear as directory or file names in the stores are <Tooltip term="slugified">Characters outside `[a-zA-Z0-9._-]` are replaced with underscores. For example, `localhost:5000` becomes `localhost_5000`. This prevents the POSIX PATH separator (`:`) from corrupting environment variables that embed these paths.</Tooltip> before they become filesystem path components. Dots and hyphens are preserved so that domain names (e.g. `ghcr.io`) and semantic versions (e.g. `3.28.1`) remain readable on disk.
:::

#### Objects {#file-structure-objects}

When ocx installs a package, the actual files land in `objects/`. The critical design decision: the storage path is derived from the content's <Tooltip term="SHA-256 digest">A 64-character hex fingerprint of the exact bytes in a file or archive. The same bytes always produce the same digest; any change — even a single bit — produces a completely different one. Used here as a content address: the path is uniquely determined by what the files contain.</Tooltip>, not from the package name or tag.

<Tree>
  <Node name="~/.ocx/objects/" icon="📦" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="{repo}/" icon="📁" open-icon="📂" open>
        <Node name="sha256:abc123…/" icon="📁" open-icon="📂" open>
          <Description>one directory per unique package build</Description>
          <Node name="content/" icon="📂">
            <Description>package files — binaries, libraries, headers</Description>
          </Node>
          <Node name="metadata.json" icon="📋">
            <Description>declared env vars, supported platforms</Description>
          </Node>
          <Node name="refs/" icon="🔗">
            <Description>back-references to install symlinks — guards against deletion</Description>
          </Node>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

This <Tooltip term="content-addressed layout">A storage scheme where every item's address is derived from a cryptographic hash of its contents. The same bytes always map to the same address — accidental duplication is impossible and silent corruption is detectable.</Tooltip> has two important consequences:

- **Automatic deduplication.** If `cmake:3.28` and `cmake:latest` resolve to the same binary build, they share one directory on disk. Storage is proportional to the number of *distinct builds*, not the number of tags that reference them.
- **Immutability.** A path under `sha256:…/` never changes its contents. This makes objects safe to reference directly from scripts or cache layers that require a known, stable binary.

::: info Similar to the Nix store and Git objects
The [Nix package manager][nix] stores every package at `/nix/store/{hash}-name/` using the same principle: the path is a function of the content. This is what makes Nix derivations reproducible across machines — the same hash always means the same files. Git's internal object store (`.git/objects/`) works identically. ocx applies this model to OCI-distributed binaries.
:::

**Garbage collection via `refs/`.** The `refs/` subdirectory inside each object tracks every install symlink that currently points to it. [`ocx clean`][cmd-clean] only removes objects whose `refs/` is empty — anything still referenced by an active install is never touched.

::: details How back-references work
When `ocx install cmake:3.28` creates the symlink `installs/…/cmake/candidates/3.28 → objects/…/sha256:abc123…/content`, it simultaneously writes a corresponding back-reference entry inside `objects/…/sha256:abc123…/refs/`. Removing the symlink via [`ocx uninstall`][cmd-uninstall] removes that back-reference entry. [`ocx clean`][cmd-clean] then applies a simple rule: if `refs/` is empty, the object is orphaned and can be safely deleted. No reference counting, no graph traversal — just check whether the directory is empty.
:::

*Commands: [`ocx install`][cmd-install] adds objects; [`ocx uninstall --purge`][cmd-uninstall] removes a specific one; [`ocx clean`][cmd-clean] removes all unreferenced objects.*

#### Index {#file-structure-index}

When you run `ocx install cmake:3.28`, how does ocx know which binary to fetch? It looks up the tag `3.28` in the index and finds the corresponding <Tooltip term="digest">The content fingerprint stored in the OCI registry manifest. A tag like `3.28` is just a human-readable alias; the digest is the canonical, immutable identifier that pinpoints the exact binary build behind that tag at index-update time.</Tooltip>. The index is a local copy of that mapping — no network required.

<Tree>
  <Node name="~/.ocx/index/" icon="🗂️" open>
    <Node name="{registry}/" icon="📁" open-icon="📂" open>
      <Node name="tags/" icon="📁" open-icon="📂" open>
        <Node name="{repo}.json" icon="📄">
          <Description>{ "3.28": "sha256:abc…", "3.30": "sha256:def…" }</Description>
        </Node>
      </Node>
      <Node name="objects/" icon="📁" open-icon="📂">
        <Node name="sha256:abc123….json" icon="📄">
          <Description>cached OCI manifest — platform list, layer references</Description>
        </Node>
      </Node>
    </Node>
  </Node>
</Tree>

The index is a *snapshot*: it reflects the state of the remote registry at the last time you refreshed it. That snapshot has two benefits:

- **Offline installs.** If the index is populated and the binary is already in the object store, `ocx install cmake:3.28` works with no network access at all — tag resolution is a local file read.
- **Reproducibility.** A CI runner that does not update the index gets the same binary on every run, regardless of what the registry serves today. Tags in OCI registries are mutable; your local snapshot is not.

::: info Similar to APT's package lists
`apt-get update` downloads package metadata from configured sources and caches it in `/var/lib/apt/lists/`. All subsequent `apt-get install` calls resolve packages from that local snapshot — the network is only involved during an explicit refresh, not on every install. `ocx index update <package>` is the per-package equivalent: you control when the snapshot changes, and the rest of the time you work from the local cache.
:::

`ocx index update <package>` refreshes the index for a specific package. The global flag [`--remote`][arg-remote] skips the local index entirely and queries the registry directly for a single command — useful for a one-off check without updating the persistent snapshot.

*Commands: [`ocx index catalog`][cmd-index-catalog], [`ocx index update`][cmd-index-update]; flag: [`--remote`][arg-remote]*

#### Installs {#file-structure-installs}

Object-store paths embed the digest: `~/.ocx/objects/ocx.sh/cmake/sha256:abc123…/content`. That path changes on every upgrade. You cannot put it in a shell profile, an IDE config, or a build file and expect it to still work next month.

`installs/` solves this with <Tooltip term="stable symlinks">A symbolic link (symlink) is a filesystem entry that points to another path. The link's own path never changes — only its target can be updated. Tools that reference the link path continue to work transparently after the target is re-pointed to a new version.</Tooltip> whose paths never change — only their targets are re-pointed when you install or select a new version.

<Tree>
  <Node name="~/.ocx/installs/" icon="🔀" open>
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

**`current`** — a floating pointer to whichever candidate you last declared active. Set by [`ocx select`][cmd-select] (or `ocx install --select` in one step). It is never updated automatically — not when you install a newer version, not when you update the index. This is intentional: tools referencing `current` should only change behavior when *you* decide they should.

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
{ "cmake.cmakePath": "~/.ocx/installs/ocx.sh/cmake/current/content/bin/cmake" }
```

```sh
# ~/.bashrc — always resolves to the selected version
export PATH="$HOME/.ocx/installs/ocx.sh/cmake/current/content/bin:$PATH"
```

When you later run `ocx install --select cmake:3.32`, `current` is re-pointed. Your IDE and shell pick up the new version automatically — no config changes needed.
:::

*Commands: [`ocx install`][cmd-install], [`ocx select`][cmd-select]; [`ocx deselect`][cmd-deselect], [`ocx uninstall`][cmd-uninstall]*

### Path Resolution {#path-resolution}

Several commands resolve package content to a filesystem path that is embedded in their output.
The path you receive determines how stable that output is across future package updates.

| Mode | Flag | Path used | Auto-install | Use case |
|---|---|---|---|---|
| Object store *(default)* | *(none)* | `~/.ocx/objects/…/<digest>/content` | yes (online) | CI, scripts, one-shot queries |
| Candidate symlink | `--candidate` | `~/.ocx/installs/…/candidates/<tag>/content` | **no** | Pinning to a specific tag in editor or IDE config |
| Current symlink | `--current` | `~/.ocx/installs/…/current/content` | **no** | "Always selected" path in shell profiles or IDE settings |

**Object store** paths are content-addressed and change whenever the package digest changes (i.e. on every update).
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
  "clangd.path": "/home/user/.ocx/installs/ocx.sh/clangd/current/content/bin/clangd"
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

- **[Local index][fs-index] snapshot.** The tag → digest mapping only changes when you run `ocx index update`. The snapshot is yours until you decide to refresh it.
- **[Content-addressed object store][fs-objects].** Once a binary is installed, it lives at a path derived from its SHA-256 digest. The tag used to install it is irrelevant — the bytes are permanent.

For absolute reproducibility without relying on any index, reference the digest directly: `cmake@sha256:abc123…`
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
:::

### Locking {#versioning-locking}

The most direct lock is a digest: `cmake@sha256:abc123…` bypasses the [index][fs-index] entirely and identifies an exact binary regardless of what any tag points to. Because all installed bytes are [content-addressed][fs-objects], every package can be pinned this way — no lockfiles, no registry queries, just the hash.

For most use cases, the [local index snapshot][fs-index] already provides the lock. Tags resolve to the digest recorded at last update, and that mapping does not change until you run `ocx index update`. A CI runner that never updates its index gets the same binary on every run.

For tool authors distributing ocx-powered workflows, there is a more ergonomic option. The [local index][fs-index] holds only metadata — small JSON files, no binaries — so it can be shipped inside a [GitHub Action][github-actions-docs], [Bazel Rule][bazel-rules], or [DevContainer Feature][devcontainer-features]. The result is a two-level lock: the *tool version* locks the *index snapshot*, which locks the *resolved binary*. Users pin the tool and get deterministic builds without managing digests, platform conditionals, or separate lockfiles.

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

The local index reads from [`~/.ocx/index/`][fs-index] — a <Tooltip term="snapshot">A point-in-time copy of the remote registry's tag-to-digest mappings. No binaries — only the small JSON metadata files needed to resolve package identifiers.</Tooltip> of OCI tag-to-digest mappings. Resolving `cmake:3` is a file read; no network request is made.

**The local index is never updated automatically.** You decide when your snapshot changes. Until you explicitly refresh it, the same identifier always resolves to the same digest — on your laptop, on CI, and on every team member's machine. Rolling tags like `cmake:3` map to the digest current at last update, not whatever the registry serves today.

The snapshot is small enough — JSON metadata only, no binaries — to ship *inside* a [Bazel Rule][bazel-rules] or [GitHub Action][github-actions-docs]. Those tools bundle a frozen snapshot at release time and set [`OCX_INDEX`][env-ocx-index] (or pass [`--index`][arg-index]) to point `ocx` at it; consumers write `cmake:3` and the bundled snapshot resolves it deterministically, with the [object store][fs-objects] and [install symlinks][fs-installs] remaining in `OCX_HOME` as usual.

::: tip Out-of-the-Box Support for Dependabot and Renovate
A single version bump to the action or rule — proposed automatically by [Dependabot][dependabot] or [Renovate][renovate] — advances the bundled index. Users get the updated binary with no config changes. No [per-platform URL matrix][toolchains-llvm] to hand-edit, no separate PR to bump the tool itself.
:::

[`ocx index update <package>`][cmd-index-update] syncs the local index for a specific package from the remote registry — downloading the current tag-to-digest mappings and any missing manifests, then writing them to disk. Packages not listed are not touched.

*Commands: [`ocx index update`][cmd-index-update], [`ocx index catalog`][cmd-index-catalog], [`ocx index list`][cmd-index-list]*

### Active Index {#indices-selected}

Every command that resolves a package identifier — [`ocx install`][cmd-install], [`ocx find`][cmd-find], [`ocx exec`][cmd-exec], [`ocx index list`][cmd-index-list] — uses one working index for that invocation. By default, this is the local index. Two flags change which index is used:

| Mode | Flag | Source | Network? |
|---|---|---|---|
| Default | *(none)* | Local snapshot | No (unless fetching a new binary) |
| Remote | [`--remote`][arg-remote] | OCI registry | Yes |
| Offline | [`--offline`][arg-offline] | Local snapshot | Never |

**`--remote`** bypasses the local snapshot for a single command and queries the registry directly. The persistent local index is not updated. Use it for a one-off check — seeing current available tags, or resolving the latest digest — without committing the result to the local snapshot.

**`--offline`** prevents all network access for that command. If the local index does not have a requested package, the command fails immediately rather than attempting a registry query. Useful to verify that your current index and object store are self-sufficient before a build in a restricted or air-gapped environment.

[`--index`][arg-index] / [`OCX_INDEX`][env-ocx-index] do not change the active index mode — the local snapshot remains active. They only change *where* that snapshot is read from. See [Local Index](#indices-local).

The active index controls tag and manifest resolution only. The [object store][fs-objects] is independent — installed binaries are accessible in all three modes regardless of which index is active.

## Authentication {#authentication}

ocx utilizes a layered approach of different authentication methods.
Most of these are specific to the registry being accessed, allowing to configure different authentication methods for different registries.
The following authentication methods are supported and queried in the the order they are listed.
If one of the methods is successful, the rest will be ignored.

- [Environment variables][auth-env-vars]
- [Docker credentials][auth-docker-creds]

### Environment Variables {#authentication-environment-variables}

To configure authentication for a registry you can use environment variables.
The following examples show how to configure authentication for the registry `docker.io` using a token or a bearer token.

::: code-group
```sh [token]
export OCX_AUTH_docker_io_TYPE=token
export OCX_AUTH_docker_io_TOKEN="<token>"
```

```sh [bearer]
export OCX_AUTH_docker_io_TYPE=bearer
export OCX_AUTH_docker_io_USER="<user>"
export OCX_AUTH_docker_io_TOKEN="<token>"
```
:::

The above examples configures the following environment variables:

- [`OCX_AUTH_<REGISTRY>_TYPE`][env-auth-type]: The type of authentication to use for the registry.
- [`OCX_AUTH_<REGISTRY>_USER`][env-auth-user]: The username to use for authentication.
- [`OCX_AUTH_<REGISTRY>_TOKEN`][env-auth-token]: The password to use for authentication.

::: info
The registry is normalized by replacing all non-alphanumeric characters with underscores.
For example, for the registry `docker.io`, ocx will look for the following environment variable `OCX_AUTH_docker_io_TYPE`.
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

## CI Integration {#ci}

CI environments need tool binaries to be available and their environment variables exported — but
they don't need version switching, candidate symlinks, or any of the install-store machinery that
supports interactive use. OCX provides two commands tailored for this:

[`package pull`][cmd-package-pull] downloads packages into the content-addressed
[object store][fs-objects] without creating any symlinks. [`ci export`][cmd-ci-export] then writes
the package-declared environment variables directly into the CI system's runtime files.

```shell
ocx package pull cmake:3.28
ocx ci export cmake:3.28
```

On [GitHub Actions][github-actions-docs], `ci export` auto-detects the environment and appends
`PATH` entries to `$GITHUB_PATH` and other variables to `$GITHUB_ENV`, making them available in
all subsequent steps.

:::tip
`package pull` only touches the object store — no symlinks, no install-store mutations. This makes
it safe to run concurrently in matrix builds that share a cached [`OCX_HOME`][env-ocx-home], since
content-addressed writes are inherently idempotent.
:::

:::info Relationship to `install`
[`install`][cmd-install] is `package pull` plus candidate-symlink creation (and optionally
`--select` for the current symlink). In CI you typically don't need symlinks — the
content-addressed object-store path that `package pull` reports is fully reproducible and
digest-derived.
:::

<!-- external -->
[nix]: https://nixos.org/
[sdkman]: https://sdkman.io/
[homebrew]: https://brew.sh/
[docker-images]: https://hub.docker.com/search?image_filter=official
[semver]: https://semver.org/
[oci-image-index]: https://github.com/opencontainers/image-spec/blob/main/image-index.md
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

<!-- commands -->
[cmd-clean]: ./reference/command-line.md#clean
[cmd-deselect]: ./reference/command-line.md#deselect
[cmd-find]: ./reference/command-line.md#find
[cmd-exec]: ./reference/command-line.md#exec
[cmd-install]: ./reference/command-line.md#install
[cmd-select]: ./reference/command-line.md#select
[cmd-uninstall]: ./reference/command-line.md#uninstall
[cmd-index-catalog]: ./reference/command-line.md#index-catalog
[cmd-index-list]: ./reference/command-line.md#index-list
[cmd-index-update]: ./reference/command-line.md#index-update
[cmd-package-pull]: ./reference/command-line.md#package-pull
[cmd-package-push]: ./reference/command-line.md#package-push
[cmd-ci-export]: ./reference/command-line.md#ci-export
[arg-remote]: ./reference/command-line.md#arg-remote
[arg-offline]: ./reference/command-line.md#arg-offline
[arg-index]: ./reference/command-line.md#arg-index

<!-- environment -->
[env-ocx-home]: ./reference/environment.md#ocx-home
[env-ocx-index]: ./reference/environment.md#ocx-index
[env-auth-type]: ./reference/environment.md#ocx-auth-registry-type
[env-auth-user]: ./reference/environment.md#ocx-auth-registry-user
[env-auth-token]: ./reference/environment.md#ocx-auth-registry-token
[env-docker-config]: ./reference/environment.md#external-docker-config

<!-- internal -->
[fs-index]: #file-structure-index
[fs-installs]: #file-structure-installs
[fs-objects]: #file-structure-objects
[versioning-tags]: #versioning-tags
[auth-env-vars]: #authentication-environment-variables
[auth-docker-creds]: #authentication-docker-credentials
[faq-build-separator]: ./faq.md#versioning-build-separator
