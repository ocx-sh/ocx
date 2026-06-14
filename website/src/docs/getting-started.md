---
outline: deep
---
# Getting Started {#getting-started}

This guide walks through the core ocx workflow — from running your first package to managing multiple versions. It assumes ocx is [already installed][installation]. Each section builds on the previous one, but they can be read independently.

::: tip First-time setup
On a fresh install, your [local index][fs-index] is empty — ocx won't find any packages. Pass `--remote` to query the registry directly. Once a package is installed, commands that operate on it ([`which`][cmd-which], [`select`][cmd-select], [`env`][cmd-env], [`exec`][cmd-exec]) work without the flag. To populate your local index for browsing and offline use, run [`ocx index update`][cmd-index-update] with the packages you need.
:::

## Quick Start {#quick-start}

Most tools require a separate install step before you can use them for the first time. You configure a source, install the tool, then run it. For a one-off task, that overhead is wasteful.

[`ocx package exec`][cmd-exec] collapses that into a single command. If the package is not in the local [object store][fs-objects], it is downloaded automatically. Then the command runs inside an isolated environment containing only the package's declared variables — no ambient `PATH` pollution.

<<< @/_scripts/getting-started/exec.sh{sh}

<Terminal src="/casts/getting-started/exec.cast" title="Running a package" collapsed />

The binary is cached in the [object store][fs-objects], so subsequent calls with the same version skip the download. Nothing else persists: no [candidate symlink][fs-symlinks], no [current pointer][fs-symlinks].

::: tip
Use [`ocx package exec`][cmd-exec] for one-off tasks or in CI where you want a reproducible, isolated invocation. For tools you reach for every day, read on.
:::

::: details Running multiple packages in a single invocation
Pass multiple packages before `--`. Their environments are merged in declaration order so all tools are accessible inside the subprocess.

A real-world example: [Bun][bun] is a JavaScript runtime that complements [Node.js][nodejs]. Bringing both runtimes together with a single command means both `node` and `bun` are on `PATH` without any manual setup:

<<< @/_scripts/getting-started/exec-multi.sh{sh}

<Terminal src="/casts/getting-started/exec-multi.cast" title="Running multiple packages together" collapsed />
:::

## Installing {#installing}

Running [`ocx package exec`][cmd-exec] re-resolves the package on every invocation. For tools you use across multiple sessions — compilers, interpreters, build utilities — you want a persistent, named install that is available offline and at a known, stable path.

[`ocx package install`][cmd-install] downloads the package into the [content-addressed object store][fs-objects] and creates a [candidate symlink][fs-symlinks] at `~/.ocx/symlinks/{registry}/{repo}/candidates/{tag}`. That path never changes after installation. If two tags resolve to the same binary build, only one object lives on disk.

<<< @/_scripts/getting-started/install.sh{sh}

<Terminal src="/casts/getting-started/install.cast" title="Installing a package" collapsed />

Once installed, [`ocx package which --candidate`][cmd-which] returns the stable candidate path without re-resolving the package — useful for embedding in build scripts, IDE configs, or CI pipelines that need a fixed path pinned to a specific version:

<<< @/_scripts/getting-started/find-candidate.sh{sh}

<Terminal src="/casts/getting-started/find-candidate.cast" title="Finding an installed package" collapsed />

::: info Like `apt install` vs a one-shot download
`apt install cmake` persists the tool system-wide. A one-shot download script runs it and leaves nothing behind. `ocx package install` is the former — persistent, versioned, offline-ready — but without root, without a system-wide footprint, and with multiple versions coexisting side by side.
:::

::: tip CI workflows: `ocx package pull`
In CI pipelines, symlink management is unnecessary — you just need the binary at a known path. [`ocx package pull`][cmd-package-pull] downloads directly into the [object store][fs-objects] without creating candidate or current symlinks. To inject resolved environment variables into the runner, use `ocx --format json package env` for machine-readable output, or `ocx package env --shell=sh` to emit `export` statements directly into the runner's environment.
:::

See the [object store][fs-objects] and [install symlinks][fs-symlinks] sections of the user guide for the full three-store architecture behind this.

## Switching Versions {#switching-versions}

Installing a package creates a candidate path that includes the tag: `candidates/21`. When you upgrade to version 25, the new candidate lands at `candidates/25`. Any config referencing `candidates/21` keeps working — but also stays pinned forever, requiring a manual edit on every upgrade.

[`ocx package select`][cmd-select] introduces a second symlink level: `current`. It is a tag-free, floating pointer to whichever candidate you last declared active. Update it once with `select`, and every path referencing `current` picks up the new version automatically.

[`ocx package install --select`][cmd-install] combines installation and selection in one step:

<<< @/_scripts/getting-started/install-select.sh{sh}

<Terminal src="/casts/getting-started/install-select.cast" title="Installing and selecting a version" collapsed />

To switch between already-installed versions, or to remove the active pointer without uninstalling, use [`ocx package select`][cmd-select] and [`ocx package deselect`][cmd-deselect]:

<Terminal src="/casts/getting-started/select-deselect.cast" title="Switching and removing the active version" collapsed />

::: info Same pattern as SDKMAN and nvm
[SDKMAN][sdkman] manages Java SDKs with `sdk default java 21-tem` — a `current` symlink that re-points to the chosen version without changing `$JAVA_HOME`. [nvm][nvm]'s `nvm alias default 20` is the same idea for Node. ocx applies this same pattern to any binary package — including Java itself, as shown above — with no plugin to install and no tool-specific version manager to learn.
:::

::: tip Use `--current` paths in shell profiles and IDE settings
Because `current` never changes its own path — only its target — you can reference it once and forget it. When you run `ocx package install --select corretto:25`, `current` is re-pointed. Your IDE and shell pick up the new version automatically, with no config edits.

See [path resolution][path-resolution] in the user guide for the full comparison of object-store, candidate, and current paths.
:::

## Uninstalling {#uninstalling}

[`ocx package uninstall`][cmd-uninstall] removes the [candidate symlink][fs-symlinks] and its back-reference from the object's `refs/` directory. The binary itself is kept unless `--purge` is given and `refs/` is empty after the uninstall.

<<< @/_scripts/getting-started/uninstall.sh{sh}

<Terminal src="/casts/getting-started/uninstall.cast" title="Uninstalling a package" collapsed />

If the package was also selected, pass `--deselect` to remove the `current` pointer in the same step, or run [`ocx package deselect`][cmd-deselect] separately first.

::: warning Uninstall removes the symlink, not necessarily the binary
The [candidate symlink][fs-symlinks] is removed immediately. The binary object in the [object store][fs-objects] is preserved unless `--purge` is given and no other references remain. To remove all unreferenced objects in one pass, use [`ocx clean`][cmd-clean].
:::

See the [object store][fs-objects] section of the user guide for how the `refs/` back-reference system makes garbage collection safe.

## Environment {#environment}

Tools are more than binaries. A compiler suite needs `CC`, `CXX`, and `LD_LIBRARY_PATH`. A runtime needs `JAVA_HOME` or `PYTHONHOME`. Keeping those variables in sync with installed paths manually is fragile and collision-prone when multiple tools share a shell.

ocx packages declare their environment variables in `metadata.json`. [`ocx package env`][cmd-env] resolves those declarations relative to the installed path and prints them as a JSON document. [`ocx package exec`][cmd-exec] injects them into a clean child process automatically.

<<< @/_scripts/getting-started/env.sh{sh}

<Terminal src="/casts/getting-started/env.cast" title="Package environment" collapsed />

::: tip Persistent shell environment
OCX activation is handled by `$OCX_HOME/env.sh`, which is written by the installer and sourced from your login profile. At shell startup it invokes `ocx self activate --shell=sh` via the installer-written absolute path, so activation works even before `ocx` is on `PATH`. The command emits three blocks: a PATH prepend to the OCX binary directory, shell completions, and an `eval "$(ocx --global env --shell=sh)"` call that exports your global toolchain's environment. Because the PATH entry points at the `current` symlink, your profile stays valid across upgrades — no manual edit required.
:::

::: details Composing environments from multiple packages
Pass multiple packages to merge their environments in declaration order. Variables of type `path` — like `PATH` — are prepended to any existing value; `constant` variables replace it entirely. The result is a complete, composed environment with no manual merging.

`nodejs:24` contributes its `bin/` to `PATH`; `bun:1.3` contributes its own `bin/`. Both runtimes are available inside the subprocess without any manual export:

<<< @/_scripts/getting-started/env-multi.sh{sh}

<Terminal src="/casts/getting-started/env-multi.cast" title="Composing environments from multiple packages" collapsed />
:::

See the [`ocx package env` reference][cmd-env] for the full flag set, including `--shell` mode for emitting eval-safe export lines.

## Project Toolchain {#project-toolchain}

When a repository needs a fixed set of tools — a compiler, a formatter, a linter — you can declare them once and lock them to exact digests. Every contributor and every CI runner then gets the same binaries automatically.

Three commands set up the toolchain:

<<< @/_scripts/getting-started/project-toolchain.sh{sh}

[`ocx init`][cmd-init] writes a minimal `ocx.toml`. [`ocx add`][cmd-add] appends a binding, resolves the tag to per-platform digests, and writes `ocx.lock`. [`ocx run`][cmd-run] reads `ocx.lock` and spawns the command with the locked toolchain's environment — no manual `export` or PATH manipulation needed.

See [Pin a project's tools][user-guide-project] in the User Guide for groups, lifecycle commands, and CI setup.

## Next Steps {#next-steps}

The sections above cover the everyday workflow. For deeper topics:

- **[User Guide][user-guide]** — the [three-store architecture][fs-objects], [versioning and tag hierarchy][versioning-tags], [locking strategies][versioning-locking], and [authentication][auth] for private registries.
- **[Command Reference][cmd-ref]** — full documentation for every flag and option. Covers global flags ([`--offline`][arg-offline], [`--remote`][arg-remote], [`--index`][arg-index]), output formats, and all subcommands.
- **[Environment Reference][env-ref]** — every environment variable ocx reads, including auth configuration for private registries.

### Config file {#next-steps-config}

No setup is required — OCX works out of the box without any config file. When you want to set a persistent default (like a private registry hostname), place a `config.toml` in [`$OCX_HOME`][env-ocx-home]`/` (default: `~/.ocx/config.toml`):

```toml
[registry]
default = "registry.company.com"
```

Environment variables and CLI flags always override config values. For full details, see the [Configuration reference][config-ref].

[`ocx index catalog`][cmd-index-catalog] and [`ocx index list`][cmd-index-list] let you browse what is available in the registry before installing:

<Terminal src="/casts/getting-started/index.cast" title="Browsing available packages" collapsed />

<!-- external -->
[sdkman]: https://sdkman.io/
[nvm]: https://github.com/nvm-sh/nvm
[bun]: https://bun.sh/
[nodejs]: https://nodejs.org/

<!-- pages -->
[installation]: ./installation.md
[user-guide]: ./user-guide.md
[cmd-ref]: ./reference/command-line.md
[env-ref]: ./reference/environment.md
[config-ref]: ./reference/configuration.md

<!-- user guide sections -->
[user-guide-project]: ./user-guide.md#project

<!-- commands -->
[cmd-init]: ./reference/command-line.md#init
[cmd-add]: ./reference/command-line.md#add
[cmd-run]: ./reference/command-line.md#run
[cmd-install]: ./reference/command-line.md#package-install
[cmd-which]: ./reference/command-line.md#package-which
[cmd-exec]: ./reference/command-line.md#package-exec
[cmd-env]: ./reference/command-line.md#package-env
[cmd-select]: ./reference/command-line.md#package-select
[cmd-deselect]: ./reference/command-line.md#package-deselect
[cmd-uninstall]: ./reference/command-line.md#package-uninstall
[cmd-clean]: ./reference/command-line.md#clean
[cmd-index-catalog]: ./reference/command-line.md#index-catalog
[cmd-index-list]: ./reference/command-line.md#index-list
[cmd-index-update]: ./reference/command-line.md#index-update
[cmd-package-pull]: ./reference/command-line.md#package-pull
[arg-offline]: ./reference/command-line.md#arg-offline
[arg-remote]: ./reference/command-line.md#arg-remote
[arg-index]: ./reference/command-line.md#arg-index

<!-- environment -->
[env-ocx-home]: ./reference/environment.md#ocx-home

<!-- user guide sections -->
[fs-objects]: ./user-guide.md#file-structure-objects
[fs-symlinks]: ./user-guide.md#file-structure-symlinks
[fs-index]: ./user-guide.md#file-structure-index
[path-resolution]: ./user-guide.md#path-resolution
[versioning-tags]: ./user-guide.md#versioning-tags
[versioning-locking]: ./user-guide.md#versioning-locking
[auth]: ./user-guide.md#authentication
