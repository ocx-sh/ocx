---
outline: deep
---
# Getting Started {#getting-started}

This guide walks through the core ocx workflow — from running your first package to managing multiple versions. It assumes ocx is [already installed][installation]. Each section builds on the previous one, but they can be read independently.

## Quick Start {#quick-start}

Most tools require a separate install step before you can use them for the first time. You configure a source, install the tool, then run it. For a one-off task, that overhead is wasteful.

[`ocx exec`][cmd-exec] collapses that into a single command. If the package is not in the local [object store][fs-objects], it is downloaded automatically. Then the command runs inside an isolated environment containing only the package's declared variables — no ambient `PATH` pollution.

```sh
ocx exec uv:0.10 -- uv --version
```

<Terminal src="/casts/exec.cast" title="Running a package" collapsed />

The binary is cached in the [object store][fs-objects], so subsequent calls with the same version skip the download. Nothing else persists: no [candidate symlink][fs-installs], no [current pointer][fs-installs].

::: tip
Use [`ocx exec`][cmd-exec] for one-off tasks or in CI where you want a reproducible, isolated invocation. For tools you reach for every day, read on.
:::

::: details Running multiple packages in a single invocation
Pass multiple packages before `--`. Their environments are merged in declaration order so all tools are accessible inside the subprocess.

A real-world example: [Bun][bun] is a JavaScript runtime that complements [Node.js][nodejs]. Bringing both runtimes together with a single command means both `node` and `bun` are on `PATH` without any manual setup:

```sh
ocx exec node:22 bun:1 -- bun --version
```

<Terminal src="/casts/exec-multi.cast" title="Running multiple packages together" collapsed />
:::

## Installing {#installing}

Running [`ocx exec`][cmd-exec] re-resolves the package on every invocation. For tools you use across multiple sessions — compilers, interpreters, build utilities — you want a persistent, named install that is available offline and at a known, stable path.

[`ocx install`][cmd-install] downloads the package into the [content-addressed object store][fs-objects] and creates a [candidate symlink][fs-installs] at `~/.ocx/installs/{registry}/{repo}/candidates/{tag}`. That path never changes after installation. If two tags resolve to the same binary build, only one object lives on disk.

```sh
ocx install uv:0.10
```

<Terminal src="/casts/install.cast" title="Installing a package" collapsed />

Once installed, [`ocx find --candidate`][cmd-find] returns the stable candidate path without re-resolving the package — useful for embedding in build scripts, IDE configs, or CI pipelines that need a fixed path pinned to a specific version:

```sh
ocx find --candidate uv:0.10
```

<Terminal src="/casts/find-candidate.cast" title="Finding an installed package" collapsed />

::: info Like `apt install` vs a one-shot download
`apt install cmake` persists the tool system-wide. A one-shot download script runs it and leaves nothing behind. `ocx install` is the former — persistent, versioned, offline-ready — but without root, without a system-wide footprint, and with multiple versions coexisting side by side.
:::

See the [object store][fs-objects] and [install symlinks][fs-installs] sections of the user guide for the full three-store architecture behind this.

## Switching Versions {#switching-versions}

Installing a package creates a candidate path that includes the tag: `candidates/1`. When you upgrade to version 2, the new candidate lands at `candidates/2`. Any config referencing `candidates/1` keeps working — but also stays pinned forever, requiring a manual edit on every upgrade.

[`ocx select`][cmd-select] introduces a second symlink level: `current`. It is a tag-free, floating pointer to whichever candidate you last declared active. Update it once with `select`, and every path referencing `current` picks up the new version automatically.

[`ocx install --select`][cmd-install] combines installation and selection in one step:

```sh
ocx install --select uv:0.10
ocx find --current uv
```

<Terminal src="/casts/install-select.cast" title="Installing and selecting a version" collapsed />

To switch between already-installed versions, or to remove the active pointer without uninstalling, use [`ocx select`][cmd-select] and [`ocx deselect`][cmd-deselect]:

<Terminal src="/casts/select-deselect.cast" title="Switching and removing the active version" collapsed />

::: info Same pattern as SDKMAN and nvm
[SDKMAN][sdkman] manages Java SDKs with `sdk default java 21-tem` — a `current` symlink that re-points to the chosen version without changing `$JAVA_HOME`. [nvm][nvm]'s `nvm alias default 20` is the same idea for Node. ocx applies this pattern to any binary package, with no plugin to install and no tool-specific version manager to learn.
:::

::: tip Use `--current` paths in shell profiles and IDE settings
Because `current` never changes its own path — only its target — you can reference it once and forget it. When you run `ocx install --select uv:0.11`, `current` is re-pointed. Your IDE and shell pick up the new version automatically, with no config edits.

See [path resolution][path-resolution] in the user guide for the full comparison of object-store, candidate, and current paths.
:::

## Uninstalling {#uninstalling}

[`ocx uninstall`][cmd-uninstall] removes the [candidate symlink][fs-installs] and its back-reference from the object's `refs/` directory. The binary itself is kept unless `--purge` is given and `refs/` is empty after the uninstall.

```sh
ocx install uv:0.10
ocx uninstall uv:0.10
```

<Terminal src="/casts/uninstall.cast" title="Uninstalling a package" collapsed />

If the package was also selected, pass `--deselect` to remove the `current` pointer in the same step, or run [`ocx deselect`][cmd-deselect] separately first.

::: warning Uninstall removes the symlink, not necessarily the binary
The [candidate symlink][fs-installs] is removed immediately. The binary object in the [object store][fs-objects] is preserved unless `--purge` is given and no other references remain. To remove all unreferenced objects in one pass, use [`ocx clean`][cmd-clean].
:::

See the [object store][fs-objects] section of the user guide for how the `refs/` back-reference system makes garbage collection safe.

## Environment {#environment}

Tools are more than binaries. A compiler suite needs `CC`, `CXX`, and `LD_LIBRARY_PATH`. A runtime needs `JAVA_HOME` or `PYTHONHOME`. Keeping those variables in sync with installed paths manually is fragile and collision-prone when multiple tools share a shell.

ocx packages declare their environment variables in `metadata.json`. [`ocx env`][cmd-env] resolves those declarations relative to the installed path and prints them as a table. [`ocx exec`][cmd-exec] injects them into a clean child process automatically.

```sh
ocx env java:21
```

<Terminal src="/casts/env.cast" title="Package environment" collapsed />

For shell profile integration, [`ocx shell env`][cmd-shell-env] emits shell-specific `export` statements designed for `eval`. Pass `--current` to reference the [floating pointer][fs-installs] rather than a pinned version — the shell picks up new versions automatically whenever you run [`ocx select`][cmd-select]:

```sh
eval "$(ocx shell env --current cmake)"
```

<Terminal src="/casts/shell-env.cast" title="Shell profile integration" collapsed />

::: tip Persistent shell environment
Add `eval "$(ocx shell env --current <package>)"` to your `~/.bashrc` or `~/.zshrc`. Because `--current` resolves at shell startup, this line stays valid across upgrades — run `ocx install --select <package>:<new-version>` and open a new terminal. No edit to the profile required.
:::

::: details Composing environments from multiple packages
Pass multiple packages to merge their environments in declaration order. Variables of type `path` — like `PATH` — are appended; `constant` variables are set once; `accumulator` variables are merged. The result is a complete, composed environment with no manual merging.

`node:22` contributes its `bin/` to `PATH`; `bun:1` contributes its own `bin/`. Both runtimes are available inside the subprocess without any manual export:

```sh
ocx env node:22 bun:1
```

<Terminal src="/casts/env-multi.cast" title="Composing environments from multiple packages" collapsed />
:::

See the [`ocx env` reference][cmd-env] and [`ocx shell env` reference][cmd-shell-env] for the full flag set, including `--candidate` mode for pinning the resolved path to a specific installed version.

## Shell Profile {#shell-profile}

Adding `eval "$(ocx shell env --current <package>)"` to your shell startup works for one or two tools. By the time you have five — a compiler, a runtime, a build system, a linter, and a formatter — your `.bashrc` has five nearly-identical lines to maintain. Adding a tool means editing the profile; removing one means remembering which line to delete.

[`ocx shell profile`][cmd-shell-profile] replaces those manual lines with a declarative manifest. You tell ocx which packages belong in your shell, and ocx resolves all of them at startup in a single pass.

Add packages after installing them:

```sh
ocx install cmake:3.31
ocx install llvm:22.1
ocx shell profile add cmake:3.31 llvm:22.1
```

<Terminal src="/casts/profile.cast" title="Managing shell profile packages" collapsed />

[`ocx shell profile list`][cmd-shell-profile-list] shows what is in the profile and whether each entry's symlink is healthy:

```sh
ocx shell profile list
```

At shell startup, the ocx env file calls [`ocx shell profile load`][cmd-shell-profile-load] to resolve every profiled package and emit the right exports. Broken entries — packages that were deselected or uninstalled since they were added — are silently skipped so a stale entry never blocks your shell from starting.

To stop loading a package, remove it from the profile. The package itself stays installed:

```sh
ocx shell profile remove llvm:22.1
```

::: tip Resolution modes
By default, profiled packages resolve via the `candidates/{tag}` symlink — pinned to the specific tag you installed. To use the floating `current` symlink instead (follows [`ocx select`][cmd-select]), use `--current`:

```sh
ocx install --select cmake:3.31
ocx shell profile add --current cmake:3.31
```

For the content-addressed object store path instead, use `--content`. This resolves to the digest-based path in `~/.ocx/objects/` — precise and self-verifying, but the path changes whenever the package is reinstalled at a different version.
:::

## Next Steps {#next-steps}

The sections above cover the everyday workflow. For deeper topics:

- **[User Guide][user-guide]** — the [three-store architecture][fs-objects], [versioning and tag hierarchy][versioning-tags], [locking strategies][versioning-locking], and [authentication][auth] for private registries.
- **[Command Reference][cmd-ref]** — full documentation for every flag and option. Covers global flags ([`--offline`][arg-offline], [`--remote`][arg-remote], [`--index`][arg-index]), output formats, and all subcommands.
- **[Environment Reference][env-ref]** — every environment variable ocx reads, including auth configuration for private registries.

[`ocx index catalog`][cmd-index-catalog] and [`ocx index list`][cmd-index-list] let you browse what is available in the registry before installing:

<Terminal src="/casts/index.cast" title="Browsing available packages" collapsed />

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

<!-- commands -->
[cmd-install]: ./reference/command-line.md#install
[cmd-find]: ./reference/command-line.md#find
[cmd-exec]: ./reference/command-line.md#exec
[cmd-env]: ./reference/command-line.md#env
[cmd-select]: ./reference/command-line.md#select
[cmd-deselect]: ./reference/command-line.md#deselect
[cmd-uninstall]: ./reference/command-line.md#uninstall
[cmd-clean]: ./reference/command-line.md#clean
[cmd-shell-env]: ./reference/command-line.md#shell-env
[cmd-shell-profile]: ./reference/command-line.md#shell-profile
[cmd-shell-profile-list]: ./reference/command-line.md#shell-profile-list
[cmd-shell-profile-load]: ./reference/command-line.md#shell-profile-load
[cmd-index-catalog]: ./reference/command-line.md#index-catalog
[cmd-index-list]: ./reference/command-line.md#index-list
[arg-offline]: ./reference/command-line.md#arg-offline
[arg-remote]: ./reference/command-line.md#arg-remote
[arg-index]: ./reference/command-line.md#arg-index

<!-- user guide sections -->
[fs-objects]: ./user-guide.md#file-structure-objects
[fs-installs]: ./user-guide.md#file-structure-installs
[fs-index]: ./user-guide.md#file-structure-index
[path-resolution]: ./user-guide.md#path-resolution
[versioning-tags]: ./user-guide.md#versioning-tags
[versioning-locking]: ./user-guide.md#versioning-locking
[auth]: ./user-guide.md#authentication
