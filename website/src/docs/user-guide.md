---
outline: deep
---
# User Guide

This guide walks through the everyday tasks a user runs against OCX — install a tool, switch versions, embed a stable path, lock a CI build, run a command with its dependency environment, authenticate to a private registry, work offline. Each section is task-named and self-contained.

For first-time setup and a guided quick-start, see [Getting Started][getting-started]. For *why* the behavior is shaped the way it is — content addressing, OCI tag mechanics, environment composition, GC reachability — every section ends with **Learn more** links into the matching [In Depth][in-depth] page.

## Install a tool {#install}

The basic flow is one command:

```shell
ocx install cmake:3.28
```

OCX downloads the package, verifies its [SHA-256 digest][in-depth-storage-packages], and stores it in the [content-addressed package store][in-depth-storage-packages] under `~/.ocx/packages/`. A [candidate symlink][in-depth-storage-symlinks] (`candidates/3.28`) is created so the version is reachable by name.

Multiple versions coexist — installing `cmake:3.30` next to `cmake:3.28` adds a second candidate; nothing is overwritten. The [content-addressed layout][in-depth-storage-packages] dedups identical builds automatically: if `cmake:3.28` and `cmake:latest` resolve to the same digest, they share one directory on disk.

To run a tool *once* without keeping it installed, skip the install step entirely:

```shell
ocx exec cmake:3.28 -- cmake --version
```

[`ocx exec`][cmd-exec] downloads on demand, runs in a [clean environment][in-depth-environments], and leaves no candidate symlink behind — the binary stays in the package store but no version is selected. Useful for one-off invocations and CI where persistent state is not needed.

::: tip Learn more
[Storage In Depth][in-depth-storage] — content addressing, layer dedup, hardlink assembly.
[Versioning In Depth → Tags][in-depth-versioning-tags] — what `:3.28` actually resolves to.
[Entry Points In Depth][in-depth-entry-points] — what generated launchers do under `ocx exec`.
:::

## Choose between versions {#versions}

Once two or more versions are installed, [`ocx select`][cmd-select] picks the one that becomes "current":

```shell
ocx install cmake:3.30
ocx select cmake:3.30
```

The `current` symlink is a [floating pointer][in-depth-storage-symlinks] — it only moves when *you* select a different version. Installing a newer version does not advance `current`; updating the [tag-store snapshot][in-depth-indices-local] does not advance it either. This is intentional: tools that reference `current` should only change behavior when you decide they should.

### Tags, variants, digests

A single OCX identifier covers what / which version / how built / which platform. Specificity in the tag signals intent:

| Tag | Meaning | Resolves to after refresh |
|---|---|---|
| `cmake:3.28.1_20260216120000` | Specific build, do not re-push | Same build (publisher convention) |
| `cmake:3.28.1` | Rolling patch | Latest 3.28.1 build |
| `cmake:3.28` | Rolling minor | Latest 3.28.x build |
| `cmake:3` | Rolling major | Latest 3.x build |
| `cmake:latest` | Floating | Latest release |

For *build flavor* — debug, PGO, slim — use a [variant prefix][in-depth-versioning-variants]: `python:debug-3.12`. For *exact reproducibility regardless of any tag*, use a digest: `cmake@sha256:abc123…`. Platform is auto-detected; override with `-p, --platform` only for cross-arch installs.

`ocx index list cmake --variants` shows available variants without downloading anything.

<Terminal src="/casts/variants.cast" title="Working with variants" collapsed />

[`ocx deselect cmake`][cmd-deselect] clears `current` without uninstalling. [`ocx uninstall cmake:3.28`][cmd-uninstall] removes the candidate; pass `--purge` to remove the binary too if no other reference holds it.

::: tip Learn more
[Versioning In Depth][in-depth-versioning] — full tag hierarchy, cascade mechanics, OCI tag char rules, `_build` suffix convention, OCI Image Index multi-platform spec.
[Storage In Depth → Symlinks][in-depth-storage-symlinks] — why `current` is floating, the SDKMAN/Homebrew/`update-alternatives` analogy.
:::

## Embed a stable path in your IDE or shell {#stable-paths}

Package-store paths are content-addressed and change on every upgrade — never embed them directly in an IDE config or shell profile. Embed a [symlink][in-depth-storage-symlinks] instead. Three modes cover every case:

| Mode | Flag | Path | Auto-install | Use case |
|---|---|---|---|---|
| Package store *(default)* | *(none)* | `~/.ocx/packages/…/<digest>/` | yes (online) | CI, scripts, one-shot queries |
| Candidate symlink | `--candidate` | `~/.ocx/symlinks/…/candidates/<tag>` | **no** | Pin a specific tag in editor or IDE config |
| Current symlink | `--current` | `~/.ocx/symlinks/…/current` | **no** | "Always selected" path in shell profiles or IDE settings |

Both symlink modes target the [package root][in-depth-storage-packages] directly; consumers traverse one level in (`…/content/` for files, `…/entrypoints/` for launchers, or read `…/metadata.json`).

```jsonc
// .vscode/settings.json — path survives every upgrade
{ "cmake.cmakePath": "~/.ocx/symlinks/ocx.sh/cmake/current/content/bin/cmake" }
```

```sh
# ~/.bashrc — always resolves to the selected version
export PATH="$HOME/.ocx/symlinks/ocx.sh/cmake/current/content/bin:$PATH"
```

When `ocx install --select cmake:3.32` runs later, `current` is re-pointed and the IDE / shell pick up the new version with no config edits.

For automation, [`ocx find`][cmd-find] prints the resolved package root directly:

```shell
ocx find --current cmake          # ~/.ocx/symlinks/ocx.sh/cmake/current
ocx find --candidate cmake:3.28   # ~/.ocx/symlinks/ocx.sh/cmake/candidates/3.28
```

Both `--candidate` and `--current` fail immediately if the required symlink is absent — they never auto-install. A digest component in the identifier is rejected.

### Shell profile integration

[`ocx shell profile add`][cmd-shell-profile] declares packages whose entry points should always be on `$PATH`:

```shell
ocx shell profile add cmake:3.28 nodejs:24
```

When a profiled package declares [entry points][in-depth-entry-points] in its metadata, [`ocx shell profile load`][cmd-shell-profile] (run from `~/.bashrc`, `~/.zshrc`, etc.) appends `…/current/entrypoints` to `$PATH` so every launcher becomes a top-level command. Each launcher re-enters via [`ocx launcher exec`][cmd-launcher-exec], so the package always runs under the same [clean-environment guarantee][in-depth-environments-two-surfaces] as `ocx exec`.

::: tip Learn more
[Storage In Depth → Symlinks][in-depth-storage-symlinks] — candidate vs current design, package-root vs content traversal.
[Entry Points In Depth][in-depth-entry-points] — generated launchers, synth-PATH, cross-platform shell scripting.
[Environments In Depth][in-depth-environments] — what "clean environment" actually means.
:::

## Run a command with its dependencies {#dependencies}

Package publishers can declare that their package needs other packages to function — a web app needs a JavaScript runtime; a build tool needs a compiler. Each dependency is pinned to an exact [OCI digest][in-depth-storage-packages] by the publisher. As a user, you do not manage dependencies — OCX handles them automatically.

Pull the package; OCX [resolves the closure transitively][in-depth-dependencies-resolution]:

```shell
ocx package pull webapp:2.0
```

If `webapp:2.0` declares dependencies on `nodejs:24` and `bun:1.3`, all three packages end up in the [package store][in-depth-storage-packages]. Only `webapp:2.0` is the explicit install — the dependencies are stored but not surfaced as top-level installs.

To actually *run* the package with its dependency environments configured, use [`ocx exec`][cmd-exec]:

```shell
ocx exec webapp:2.0 -- serve --port 8080
```

[`ocx exec`][cmd-exec] [composes the environments][in-depth-environments-composition-order] of all dependencies in topological order before launching the command. [`ocx env`][cmd-env] exports the same composed environment for use in your own shell.

::: warning install + select does not set up dependency environments
[`ocx install --select`][cmd-install] creates a [current symlink][in-depth-storage-symlinks] that points at the package's content directory. If you or another tool invokes a binary through that symlink directly, the dependency environments are **not** configured — only the package's own files are reachable. For packages with dependencies, always use [`ocx exec`][cmd-exec], or [`ocx env`][cmd-env] / [`ocx shell env`][cmd-shell-env] to export the full environment first.
:::

### Inspecting the dependency tree

[`ocx deps`][cmd-deps] shows the declared relationships. The default tree view annotates [non-public dependencies][in-depth-environments-visibility] so you can see at a glance which deps cross the [interface surface][in-depth-environments-two-surfaces]:

<Terminal src="/casts/deps.cast" title="Inspecting the dependency tree" collapsed />

`--flat` shows the resolved evaluation order — the exact sequence OCX uses when composing environments. This is the primary debugging tool when env vars are not what you expect:

<Terminal src="/casts/deps-flat.cast" title="Resolved dependency order" collapsed />

`--why` traces the path from a root package to a transitive dependency:

<Terminal src="/casts/deps-why.cast" title="Tracing why a dependency is pulled in" collapsed />

### Conflict warnings

If two dependencies set the same scalar variable (e.g., both set `JAVA_HOME` to different paths), OCX applies [last-writer-wins][in-depth-environments-last-wins] semantics and emits a warning. Inspect the order with [`ocx deps --flat`][cmd-deps] and decide whether the conflict is real. The same situation arises with the [shell profile][cmd-shell-profile]: if you add a package with dependencies and also have one of those dependencies in the profile as a standalone tool, both contribute env vars — either remove the standalone entry (the dependency provides it) or accept the profile entry's value.

::: tip Learn more
[Dependencies In Depth][in-depth-dependencies] — transitive resolution algorithm, scope philosophy (no version ranges, no auto-update).
[Environments In Depth][in-depth-environments] — composition order, visibility model (sealed/private/public/interface), `--self` flag, last-writer-wins.
:::

## Lock and reproduce builds {#lock}

Reproducibility in OCX has three levels, each stricter than the last.

**Pin the digest.** The strongest lock: `cmake@sha256:abc123…` bypasses [tag resolution][in-depth-indices] entirely. The bytes are content-addressed; the digest *is* the binary. Every package can be pinned this way — no lockfiles, no registry queries, just the hash.

**Pin the snapshot.** The next-strongest lock — and the one most users want — is to freeze the [local tag-to-digest snapshot][in-depth-indices-local]. Tags resolve to whatever digest was recorded at the last [`ocx index update`][cmd-index-update]; that mapping does not change until you refresh. A CI runner that never refreshes its snapshot gets the same binary on every run, even if the registry re-pushes the tag.

**Pin a bundled snapshot.** The most ergonomic lock for tool authors. The local snapshot holds only metadata — small JSON files, no binaries — so it can be shipped *inside* a [GitHub Action][github-actions-docs], [Bazel rule][bazel-rules], or [DevContainer feature][devcontainer-features]. Pinning the action version pins the snapshot, which pins the binary:

```yaml
- uses: ocx-actions/setup-cmake@v2.1.0   # pins action → pins index → pins binary
  with:
    version: "3.28"
```

A version bump to the action — proposed automatically by [Dependabot][dependabot] or [Renovate][renovate] — advances the bundled index. Users get the updated binary with no config changes. The contrast with maintaining a [hand-curated URL matrix][toolchains-llvm] (one `filename → checksum` entry per `version × os × arch`) is stark.

::: tip Learn more
[Indices In Depth → Bundled Snapshots][in-depth-indices-bundled] — full bundled-snapshot pattern, [`OCX_INDEX`][env-ocx-index] env var, Dependabot/Renovate flow.
[Versioning In Depth → Locking][in-depth-versioning-locking] — digest pin rationale, OCI tag mutability.
:::

## Use OCX in CI {#ci}

CI environments need tool binaries available with their environment variables exported — but they do not need [version switching][in-depth-storage-symlinks], candidate symlinks, or any of the install-store machinery that supports interactive use. OCX provides two CI-tailored commands:

```shell
ocx package pull cmake:3.28
ocx ci export cmake:3.28
```

[`package pull`][cmd-package-pull] downloads packages into the [content-addressed package store][in-depth-storage-packages] without creating any symlinks. [`ci export`][cmd-ci-export] writes the package-declared environment variables directly into the CI system's runtime files.

On [GitHub Actions][github-actions-docs], `ci export` auto-detects the environment and appends `PATH` entries to `$GITHUB_PATH` and other variables to `$GITHUB_ENV`, making them available in all subsequent steps.

::: tip Concurrent matrix builds
`package pull` only touches the package store — no symlinks, no symlink-store mutations. This makes it safe to run concurrently in matrix builds that share a cached [`OCX_HOME`][env-ocx-home]; [content-addressed writes][in-depth-storage-packages] are inherently idempotent.
:::

::: info Relationship to `install`
[`install`][cmd-install] is `package pull` plus candidate-symlink creation (and optionally `--select` for the current symlink). In CI, the [content-addressed package-store path][in-depth-storage-packages] that `package pull` reports is fully reproducible and digest-derived — symlinks add no value.
:::

::: tip Learn more
[Indices In Depth → Bundled Snapshots][in-depth-indices-bundled] — pair `package pull` + `ci export` with a bundled snapshot for end-to-end determinism.
[Storage In Depth → Packages][in-depth-storage-packages] — why concurrent CI writes are safe.
:::

## Authenticate with a private registry {#authentication}

OCX uses a layered approach to authentication. Most methods are scoped per registry, so different registries can use different credentials. Methods are queried in order; the first one to succeed wins:

1. [Environment variables](#authentication-environment-variables)
2. [Docker credentials](#authentication-docker-credentials)

### Environment variables {#authentication-environment-variables}

Configure auth for a registry via `OCX_AUTH_*` variables:

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

The variables are:

- [`OCX_AUTH_<REGISTRY>_TYPE`][env-auth-type] — type of authentication (`bearer` or `basic`).
- [`OCX_AUTH_<REGISTRY>_USER`][env-auth-user] — username (basic only).
- [`OCX_AUTH_<REGISTRY>_TOKEN`][env-auth-token] — password or token.

::: info Registry name normalization
The registry name in the variable is normalized by replacing all non-alphanumeric characters with underscores. For `docker.io`, OCX looks for `OCX_AUTH_docker_io_TYPE`. This is stricter than the [path component encoding][in-depth-storage-stores] used for filesystem paths, which preserves dots and hyphens.
:::

If `TYPE` is omitted but `USER` or `TOKEN` are set, OCX infers the type — both fields means basic, token alone means bearer. See the [environment variable reference][env-auth-type] for the full set.

### Docker credentials {#authentication-docker-credentials}

If a Docker configuration is found, OCX uses the stored credentials. The configuration is typically at `~/.docker/config.json` and managed via:

```sh
docker login "<registry>"
```

Override the location with [`DOCKER_CONFIG`][env-docker-config].

::: tip Learn more
[Environment variable reference][env-ref] — every `OCX_AUTH_*` variant, complete normalization rules.
[Configuration In Depth][in-depth-configuration] — pair auth env vars with per-tier config defaults.
:::

## Work offline {#offline}

Once the [local snapshot][in-depth-indices-local] is populated and the [package store][in-depth-storage-packages] holds the binaries you need, OCX runs without network access. Two flags control how strictly the network is avoided:

| Mode | Flag | Source | Network? |
|---|---|---|---|
| Default | *(none)* | Local snapshot | No (unless fetching a new binary) |
| Remote | [`--remote`][arg-remote] | OCI registry | Yes |
| Offline | [`--offline`][arg-offline] | Local snapshot | Never |

`--offline` prevents any network access for that command. If the local snapshot does not have a requested package, the command fails immediately rather than attempting a registry query — useful to verify the current snapshot and object store are self-sufficient before a build in a restricted or air-gapped environment.

`--remote` queries the live registry directly without committing the result to the local snapshot. Use it for one-off checks of currently available tags.

[`--index`][arg-index] / [`OCX_INDEX`][env-ocx-index] only change *where* the snapshot is read from — useful when consuming a [bundled snapshot][in-depth-indices-bundled] from a GitHub Action or Bazel rule.

### Refreshing the snapshot

[`ocx index update <package>`][cmd-index-update] syncs the local snapshot for a specific package:

- **Bare identifier** (e.g., `cmake`) — downloads all tag-to-digest mappings.
- **Tagged identifier** (e.g., `cmake:3.28`) — fetches only that single tag, ideal for lockfile workflows.

On a fresh machine, [`ocx install cmake:3.28`][cmd-install] does not need an explicit `index update` first — when the local snapshot has no entry for the requested tag, OCX resolves it transparently against the registry, persists it, and proceeds with the install.

::: tip Learn more
[Indices In Depth][in-depth-indices] — remote query path, snapshot internals, blob write-through caching, fresh-machine fallback.
[Versioning In Depth → Locking][in-depth-versioning-locking] — why offline snapshots are also a lock.
:::

## Configure OCX defaults {#configuration}

OCX behavior is controlled at three layers: config files, environment variables, and CLI flags. Higher layers always win — CLI flags override env vars, which override config files.

Config files are in [TOML][toml] format and live in three locations:

| Tier | Path |
|------|------|
| System | `/etc/ocx/config.toml` |
| User (Linux) | [`$XDG_CONFIG_HOME`][xdg-basedir]`/ocx/config.toml` or `~/.config/ocx/config.toml` |
| User (macOS) | `~/Library/Application Support/ocx/config.toml` (`XDG_CONFIG_HOME` is not consulted on macOS) |
| OCX home | [`$OCX_HOME`][env-ocx-home]`/config.toml` (default: `~/.ocx/config.toml`) |

Files are loaded lowest-to-highest and merged. Missing files are silently skipped. No config file is required.

**Explicit additions.** [`--config FILE`][arg-config] or [`OCX_CONFIG`][env-config]`=/path/to/file.toml` layers an extra file on top of the discovered chain — useful for refining ambient config without rewriting it. Both can be set together (`--config` sits at highest file-tier precedence). The specified file must exist. To disable an ambient `OCX_CONFIG` without unsetting it, set it to the empty string.

**Kill switch.** [`OCX_NO_CONFIG`][env-no-config]`=1` skips the discovered chain (system, user, `$OCX_HOME`) but leaves explicit paths intact. Combine with `--config` for a fully hermetic CI load: `OCX_NO_CONFIG=1 ocx --config ci.toml ...`.

::: tip Learn more
[Configuration reference][config-ref] — every config key, type, default, error string.
[Configuration In Depth][in-depth-configuration] — discovery tier rationale, merge semantics, worked examples (Docker base image, hermetic CI, portable install).
:::

## Remove and clean up {#cleanup}

[`ocx uninstall cmake:3.28`][cmd-uninstall] removes the candidate symlink for that tag. The binary stays in the [package store][in-depth-storage-packages] in case other references hold it. Pass `--purge` to also drop the binary if no [other reference][in-depth-storage-gc] remains.

[`ocx clean`][cmd-clean] sweeps the entire store — packages with no live install symlink and no [forward-ref][in-depth-storage-gc] from a dependent package are removed in a single pass, along with any layers and blobs that become unreachable.

::: tip Learn more
[Storage In Depth → Garbage Collection][in-depth-storage-gc] — full reachability walk across `refs/symlinks/`, `refs/deps/`, `refs/layers/`, `refs/blobs/`.
[Dependencies In Depth → Garbage Collection][in-depth-dependencies-gc] — why dependencies are protected by dependents, not by back-references.
:::

<!-- external -->
[toml]: https://toml.io/
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[bazel-rules]: https://bazel.build/extending/rules
[devcontainer-features]: https://containers.dev/implementors/features/
[dependabot]: https://docs.github.com/en/code-security/dependabot/working-with-dependabot/keeping-your-actions-up-to-date-with-dependabot
[renovate]: https://docs.renovatebot.com/
[toolchains-llvm]: https://github.com/bazel-contrib/toolchains_llvm/blob/master/toolchain/internal/llvm_distributions.bzl

<!-- commands -->
[arg-config]: ./reference/command-line.md#arg-config
[cmd-clean]: ./reference/command-line.md#clean
[cmd-deselect]: ./reference/command-line.md#deselect
[cmd-find]: ./reference/command-line.md#find
[cmd-exec]: ./reference/command-line.md#exec
[cmd-launcher-exec]: ./reference/command-line.md#exec
[cmd-install]: ./reference/command-line.md#install
[cmd-select]: ./reference/command-line.md#select
[cmd-uninstall]: ./reference/command-line.md#uninstall
[cmd-index-update]: ./reference/command-line.md#index-update
[cmd-package-pull]: ./reference/command-line.md#package-pull
[cmd-ci-export]: ./reference/command-line.md#ci-export
[cmd-deps]: ./reference/command-line.md#deps
[cmd-env]: ./reference/command-line.md#env
[cmd-shell-env]: ./reference/command-line.md#shell-env
[cmd-shell-profile]: ./reference/command-line.md#shell-profile
[arg-remote]: ./reference/command-line.md#arg-remote
[arg-offline]: ./reference/command-line.md#arg-offline
[arg-index]: ./reference/command-line.md#arg-index

<!-- environment -->
[env-ocx-home]: ./reference/environment.md#ocx-home
[env-ocx-index]: ./reference/environment.md#ocx-index
[env-config]: ./reference/environment.md#ocx-config
[env-no-config]: ./reference/environment.md#ocx-no-config
[env-auth-type]: ./reference/environment.md#ocx-auth-registry-type
[env-auth-user]: ./reference/environment.md#ocx-auth-registry-user
[env-auth-token]: ./reference/environment.md#ocx-auth-registry-token
[env-docker-config]: ./reference/environment.md#external-docker-config
[xdg-basedir]: ./reference/environment.md#external-xdg-config-home
[env-ref]: ./reference/environment.md

<!-- reference pages -->
[config-ref]: ./reference/configuration.md
[getting-started]: ./getting-started.md

<!-- in-depth -->
[in-depth]: ./in-depth/storage.md
[in-depth-storage]: ./in-depth/storage.md
[in-depth-storage-stores]: ./in-depth/storage.md#stores
[in-depth-storage-packages]: ./in-depth/storage.md#packages
[in-depth-storage-symlinks]: ./in-depth/storage.md#symlinks
[in-depth-storage-gc]: ./in-depth/storage.md#gc
[in-depth-versioning]: ./in-depth/versioning.md
[in-depth-versioning-tags]: ./in-depth/versioning.md#tags
[in-depth-versioning-variants]: ./in-depth/versioning.md#variants
[in-depth-versioning-locking]: ./in-depth/versioning.md#locking
[in-depth-indices]: ./in-depth/indices.md
[in-depth-indices-local]: ./in-depth/indices.md#local
[in-depth-indices-bundled]: ./in-depth/indices.md#bundled
[in-depth-dependencies]: ./in-depth/dependencies.md
[in-depth-dependencies-resolution]: ./in-depth/dependencies.md#resolution
[in-depth-dependencies-gc]: ./in-depth/dependencies.md#gc
[in-depth-environments]: ./in-depth/environments.md
[in-depth-environments-two-surfaces]: ./in-depth/environments.md#two-surfaces
[in-depth-environments-visibility]: ./in-depth/environments.md#visibility-views
[in-depth-environments-composition-order]: ./in-depth/environments.md#composition-order
[in-depth-environments-last-wins]: ./in-depth/environments.md#last-wins
[in-depth-entry-points]: ./in-depth/entry-points.md
[in-depth-configuration]: ./in-depth/configuration.md
