---
outline: deep
---
# User Guide

This guide walks through the everyday tasks a user runs against OCX — install a tool, switch versions, embed a stable path, lock a CI build, run a command with its dependency environment, authenticate to a private registry, work offline. Each section is task-named and self-contained.

For first-time setup and a guided quick-start, see [Getting Started][getting-started]. For *why* the behavior is shaped the way it is — content addressing, OCI tag mechanics, environment composition, GC reachability — every section ends with **Learn more** links into the matching [In Depth][in-depth] page.

## Install a tool {#install}

The basic flow is one command:

<<< @/_scripts/user-guide/install.sh{sh}

OCX downloads the package, verifies its [SHA-256 digest][in-depth-storage-packages], and stores it in the [content-addressed package store][in-depth-storage-packages] under `~/.ocx/packages/`. A [candidate symlink][in-depth-storage-symlinks] (`candidates/3.28`) is created so the version is reachable by name.

Multiple versions coexist — installing `cmake:3.30` next to `cmake:3.28` adds a second candidate; nothing is overwritten. The [content-addressed layout][in-depth-storage-packages] dedups identical builds automatically: if `cmake:3.28` and `cmake:latest` resolve to the same digest, they share one directory on disk.

To run a tool *once* without keeping it installed, skip the install step entirely:

<<< @/_scripts/user-guide/exec-once.sh{sh}

[`ocx package exec`][cmd-package-exec] downloads on demand, runs in a [clean environment][in-depth-environments], and leaves no candidate symlink behind — the binary stays in the package store but no version is selected. Useful for one-off invocations and CI where persistent state is not needed.

::: tip Learn more
[Storage In Depth][in-depth-storage] — content addressing, layer dedup, hardlink assembly.
[Versioning In Depth → Tags][in-depth-versioning-tags] — what `:3.28` actually resolves to.
[Entry Points In Depth][in-depth-entry-points] — what generated launchers do under `ocx package exec`.
:::

## Install without curl | sh {#install-bare-binary}

Some CI environments and security policies forbid piping a network request directly into a shell interpreter. OCX supports a fully equivalent setup path: download the binary from [GitHub Releases][github-releases-ocx], then run `ocx self setup` from the downloaded file. The result is identical to running the install script.

The goal here is getting from "I have a trusted binary" to "shell integration is complete" without any shell script involved.

### What the setup command does {#install-bare-binary-what}

`ocx self setup` performs three steps in strict order:

1. **Bootstraps itself** — installs the latest published `ocx.sh/ocx/cli` into the [content-addressed package store][in-depth-storage-packages] and wires the `current` symlink. The loose binary you downloaded is only needed to run this step; after it completes, the managed copy in `~/.ocx/` takes over.
2. **Writes the env shims** — creates `$OCX_HOME/env.sh`, `env.fish`, `env.ps1`, `env.nu`, and `env.elv`. These files are byte-identical across users; no install-time substitution occurs.
3. **Injects a source line** — adds a fenced block-marker to each detected shell profile (`.bash_profile`, `.zprofile`, `.bashrc`, `.zshrc`, `$PROFILE`, and the equivalent files for fish, nushell, and elvish). The fence is idempotent: re-running `ocx self setup` is safe.

If the bootstrap fails (for example, the registry is unreachable), the command returns a non-zero exit code and writes nothing — no partial state.

### POSIX (Linux, macOS) {#install-bare-binary-posix}

```sh
# 1. Download the binary for your platform from GitHub Releases.
#    Example: Linux x86_64.
curl -fSL https://github.com/ocx-sh/ocx/releases/latest/download/ocx-linux-amd64 \
     -o /tmp/ocx
chmod +x /tmp/ocx

# 2. Run setup. The binary bootstraps the managed copy and wires shell profiles.
/tmp/ocx self setup

# 3. Reload your shell (or open a new terminal).
source ~/.bash_profile   # bash — or ~/.zprofile for zsh
```

After step 2, the managed `ocx` binary is in `~/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin/`. The temporary binary at `/tmp/ocx` can be deleted.

### Windows (PowerShell) {#install-bare-binary-windows}

```powershell
# 1. Download the binary.
Invoke-WebRequest -Uri https://github.com/ocx-sh/ocx/releases/latest/download/ocx-windows-amd64.exe `
    -OutFile "$env:TEMP\ocx.exe"

# 2. Run setup. Writes env.ps1 and a fenced block-marker to $PROFILE.
& "$env:TEMP\ocx.exe" self setup

# 3. Reload your profile.
. $PROFILE
```

::: warning Windows execution policy
If PowerShell's execution policy is set to `Restricted`, the sourced `env.ps1` file will not run even after `ocx self setup` completes. The command reports a non-fatal advisory when it detects this. To fix it:

```powershell
Set-ExecutionPolicy -Scope CurrentUser RemoteSigned
```

`ocx self setup` never changes the execution policy automatically — that is a user security decision.
:::

### Options {#install-bare-binary-options}

| Flag | Effect |
|------|--------|
| `--no-modify-path` | Write shims only; do not touch any shell profile. Equivalent to setting [`OCX_NO_MODIFY_PATH=1`][env-no-modify-path] for that invocation. |
| `--profile PATH` | Target an explicit profile file instead of auto-detecting. Repeatable. |
| `--dry-run` | Show what would be written without writing anything. Useful to preview which profiles are detected. |
| `--force` | Overwrite a fenced block whose contents have been manually edited. |

If you plan to manage your own PATH (CI jobs, container images, package-manager installs), pass `--no-modify-path` to stop `ocx self setup` from touching any profile:

```sh
/tmp/ocx self setup --no-modify-path
# Then add ~/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin to PATH yourself.
```

Note that `--no-modify-path` is **not remembered** between invocations. If you run `ocx self setup` again later without the flag, profiles will be modified. Set `OCX_NO_MODIFY_PATH=1` persistently in your environment or pass the flag each time to prevent that. See the [environment reference][env-no-modify-path] for the full semantics.

### Install a pinned ocx version {#install-bare-binary-pin}

CI pipelines often need a specific ocx release — not "whatever is latest" — so that every runner runs the same build regardless of when the job triggers.

Pass a version to `ocx self setup` to install exactly that release. The optional `VERSION` argument accepts a tag, a content digest, or both:

```sh
# Install a specific release by tag:
/tmp/ocx self setup 0.9.2

# Install a specific release and assert the exact content (strongest guarantee):
/tmp/ocx self setup 0.9.2@sha256:ab12cd34ef56...

# Install by digest alone — no tag resolution:
/tmp/ocx self setup sha256:ab12cd34ef56...
```

The `tag@digest` form is an immutability assertion. If the tag ever resolves to different content, the command fails with exit 65 and names both digests. To get the digest value, capture it from the JSON output of a prior run:

```sh
digest=$(/tmp/ocx --format json self setup 0.9.2 | jq -r .bootstrap.digest)
```

That digest round-trips: `/tmp/ocx self setup 0.9.2@$digest` on the next run either confirms the install is already present (exit 0, status `already_present`) or downloads and verifies the exact same content.

When the pinned version is older than what is already installed, a warning appears on stderr and the downgrade proceeds. This is a signal for CI logs, not a block.

::: tip Learn more
[`ocx self setup` reference][cmd-self-setup] — full `VERSION` grammar, JSON output shape, and all exit codes.
:::

::: tip Learn more
[Shell Activation Files reference][env-shell-activation-files] — what the env shim files contain and how each shell sources them.
[`OCX_NO_MODIFY_PATH` reference][env-no-modify-path] — truthy semantics, per-invocation behavior.
[`OCX_HOME` reference][env-ocx-home] — choose a non-default install root before running setup.
:::

## Choose between versions {#versions}

Once two or more versions are installed, [`ocx package select`][cmd-package-select] picks the one that becomes "current":

<<< @/_scripts/user-guide/versions-select.sh{sh}

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

<Terminal src="/casts/user-guide/variants.cast" title="Working with variants" collapsed />

[`ocx package deselect cmake`][cmd-package-deselect] clears `current` without uninstalling. [`ocx package uninstall cmake:3.28`][cmd-package-uninstall] removes the candidate; pass `--purge` to remove the binary too if no other reference holds it.

::: tip Learn more
[Versioning In Depth][in-depth-versioning] — full tag hierarchy, cascade mechanics, OCI tag char rules, `_build` suffix convention, OCI Image Index multi-platform spec.
[Storage In Depth → Symlinks][in-depth-storage-symlinks] — why `current` is floating, the SDKMAN/Homebrew/`update-alternatives` analogy.
:::

### Namespaces {#namespaces}

The repository half of an [identifier][oci-identifier] is a path, not a single word — `registry/namespace…/name`. OCX uses this to separate what it ships from what it mirrors.

Mirrored upstream tools sit at the registry root under their common name: `cmake`, `shellcheck`, `uv`. OCX's own first-party binaries live under the reserved `ocx/` namespace — the CLI is `ocx/cli`, the mirror tool is `ocx/mirror`. The namespace *is* the provenance: a root name is an upstream tool OCX repackaged; an `ocx/` name is OCX itself.

Slash-nested names are ordinary OCI repositories — `ocx install ocx/mirror:1` resolves exactly like `ocx install cmake:3.28`. Anyone publishing to their own registry can group packages the same way; the convention is OCX's, the mechanism is the registry's.

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

When `ocx package install --select cmake:3.32` runs later, `current` is re-pointed and the IDE / shell pick up the new version with no config edits.

:::tip Prefer `ocx env` for shells
The hand-written `export PATH=…/current/content/bin` above is an escape hatch for tools that cannot evaluate shell at startup (IDEs, JSON config files). For interactive shells and project envs, prefer [`ocx env`][cmd-env-root] (toolchain-tier) or [`ocx package env`][cmd-package-env] (per-package) — they compose the full env, not just `PATH`, and stay forward-compatible if the package adds new env entries on upgrade.
:::

For automation, [`ocx package which`][cmd-which] prints the resolved package root directly:

<<< @/_scripts/user-guide/stable-paths-which.sh{sh}

Both `--candidate` and `--current` fail immediately if the required symlink is absent — they never auto-install. A digest component in the identifier is rejected.

### Running an installed tool on Windows {#stable-paths-windows}

On Windows, `ocx install` (and `ocx select`) generates two files per entrypoint in the package's `entrypoints/` directory:

| File | Role |
|------|------|
| `<name>.exe` | Native launcher — the sole Windows entry point for all callers |
| `<name>.shim` | One-line sidecar carrying the absolute package root |

There is no `.cmd` launcher. `.EXE` is unconditionally present in the default Windows [`PATHEXT`][windows-pathext], so bare-name resolution in `cmd.exe`, [PowerShell][powershell-docs], and [Git Bash][git-bash] all find `<name>.exe` with no `PATHEXT` configuration ever needed.

The `.exe` shim reads `<name>.shim` at invocation time, then calls `CreateProcessW` directly to spawn `ocx launcher exec`. It does not route through `cmd.exe`. This is the definitive fix for the [BatBadBut / CVE-2024-24576][batbadbut-advisory] class of argument-injection vulnerability — caller arguments never pass through a second `cmd.exe` parse.

:::info How the shim reaches `ocx`
The shim resolves `ocx` using `OCX_BINARY_PIN` if the variable is **defined** in the environment (even if empty), and falls back to `PATH`-resolved `ocx` only when the variable is completely unset — see [`OCX_BINARY_PIN`][env-ocx-binary-pin] for details.
:::

**Unsigned shim note.** The committed shim blobs (~138 KiB x86_64 / ~128 KiB aarch64) are unsigned in this release. [Authenticode][authenticode] signing via [SignPath Foundation][signpath] is a documented follow-on step. For backend-automation use (CI, Bazel, devcontainers), the unsigned shim is fully functional; SmartScreen friction applies only to interactive end-user downloads.

::: tip Learn more
[Entry Points In Depth][in-depth-entry-points] — launcher ABI, `launcher exec` wire protocol, clean-env execution.
[`OCX_BINARY_PIN` reference][env-ocx-binary-pin] — pin a specific `ocx` binary for nested invocations.
:::

### direnv integration {#stable-paths-direnv}

For [direnv][direnv]-driven projects, [`ocx direnv init`][cmd-direnv-init] writes an `.envrc` file that calls [`ocx direnv export`][cmd-direnv-export] on each `cd`. The stateless export block is re-evaluated by [direnv][direnv] whenever `ocx.toml` or `ocx.lock` changes.

<<< @/_scripts/user-guide/direnv.sh{sh}

This routes through the [project toolchain][in-depth-project], so the tools on `$PATH` match exactly the digests locked in `ocx.lock`. No ambient installs or manual `export` statements needed.

::: tip Learn more
[Storage In Depth → Symlinks][in-depth-storage-symlinks] — candidate vs current design, package-root vs content traversal.
[Entry Points In Depth][in-depth-entry-points] — generated launchers, synth-PATH, cross-platform shell scripting.
[Environments In Depth][in-depth-environments] — what "clean environment" actually means.
:::

## Run a command with its dependencies {#dependencies}

Package publishers can declare that their package needs other packages to function — a web app needs a JavaScript runtime; a build tool needs a compiler. Each dependency is pinned to an exact [OCI digest][in-depth-storage-packages] by the publisher. As a user, you do not manage dependencies — OCX handles them automatically.

Pull the package; OCX [resolves the closure transitively][in-depth-dependencies-resolution]:

<<< @/_scripts/user-guide/deps-pull.sh{sh}

If `webapp:2.0` declares dependencies on `nodejs:24` and `bun:1.3`, all three packages end up in the [package store][in-depth-storage-packages]. Only `webapp:2.0` is the explicit install — the dependencies are stored but not surfaced as top-level installs.

To actually *run* the package with its dependency environments configured, use [`ocx package exec`][cmd-package-exec]:

<<< @/_scripts/user-guide/deps-exec.sh{sh}

[`ocx package exec`][cmd-package-exec] [composes the environments][in-depth-environments-composition-order] of all dependencies in topological order before launching the command. [`ocx package env`][cmd-package-env] exports the same composed environment for use in your own shell.

::: warning install + select does not set up dependency environments
[`ocx package install --select`][cmd-package-install] creates a [current symlink][in-depth-storage-symlinks] that points at the package's content directory. If you or another tool invokes a binary through that symlink directly, the dependency environments are **not** configured — only the package's own files are reachable. For packages with dependencies, always use [`ocx package exec`][cmd-package-exec], or [`ocx package env`][cmd-package-env] / [`ocx env`][cmd-env] to export the full environment first.
:::

### Inspecting the dependency tree

[`ocx package deps`][cmd-deps] shows the declared relationships. The default tree view annotates [non-public dependencies][in-depth-environments-visibility] so you can see at a glance which deps cross the [interface surface][in-depth-environments-two-surfaces]:

<Terminal src="/casts/user-guide/deps.cast" title="Inspecting the dependency tree" collapsed />

`--flat` shows the resolved evaluation order — the exact sequence OCX uses when composing environments. This is the primary debugging tool when env vars are not what you expect:

<Terminal src="/casts/user-guide/deps-flat.cast" title="Resolved dependency order" collapsed />

`--why` traces the path from a root package to a transitive dependency:

<Terminal src="/casts/user-guide/deps-why.cast" title="Tracing why a dependency is pulled in" collapsed />

### Conflict warnings

If two dependencies set the same scalar variable (e.g., both set `JAVA_HOME` to different paths), OCX applies [last-writer-wins][in-depth-environments-last-wins] semantics and emits a warning. Inspect the order with [`ocx package deps --flat`][cmd-deps] and decide whether the conflict is real. The same situation arises with project toolchains: if you add a package with dependencies and also declare one of those dependencies as a top-level binding in `ocx.toml`, both contribute env vars — either remove the redundant binding (the transitive dependency provides it) or accept the top-level entry's value winning the conflict.

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

## Keep everyday tools available everywhere {#global-toolchain}

You want `ripgrep`, `cmake`, and `shellcheck` available in every shell you open — but you also want project builds to be reproducible and immune to whatever you have installed globally. These two goals conflict unless there is a hard boundary between them.

The global toolchain is that boundary. It gives you an `apt`-style "tools I always want around" set without letting any of those tools leak into a project's resolved environment.

### Adding tools to the global toolchain {#global-toolchain-add}

Use the root `--global` flag (before the subcommand) to target `$OCX_HOME/ocx.toml`:

<<< @/_scripts/user-guide/global-add.sh{sh}

`ocx --global add` records the binding in `$OCX_HOME/ocx.toml`, re-locks, installs, and selects the package in one step. Because a tool must be on PATH to be useful globally, select is always implied.

The same root `--global` flag works with `remove`, `lock`, `upgrade`, and `pull`:

<<< @/_scripts/user-guide/global-manage.sh{sh}

The global file lives at `$OCX_HOME/ocx.toml` (default `~/.ocx/ocx.toml`). Mutators create it automatically on first use — no `ocx init` step required.

::: warning `--global` and `--project` are mutually exclusive
Both flags pick a project file. Passing them together exits with code 64 (`UsageError`).
:::

### Shell activation for global tools {#global-toolchain-shell}

The OCX installer writes a thin shim file — `$OCX_HOME/env.sh` — and a single idempotent source line in the login profile. The shim calls [`ocx self activate`][cmd-self-activate] at runtime, so its content is byte-identical across users and survives `OCX_HOME` changes without re-running the installer.

`ocx self activate` emits `PATH` prepend, shell completions (unless [`OCX_NO_COMPLETIONS=1`][env-ocx-no-completions]), and an `eval "$(ocx --global env --shell=sh)"` call for the global toolchain. Every new login shell runs this block, placing the currently-selected OCX and its installed global tools on `PATH`.

The installer appends a block-marker source line to the login profile so re-running it is idempotent:

```sh
# BEGIN ocx
. "$HOME/.ocx/env.sh"
# END ocx
```

You can inspect what the global env exports:

<<< @/_scripts/user-guide/global-env.sh{sh}

`--shell` is the only eval-safe output channel. Do not `eval "$(ocx --global env)"` — plain table output is not sourceable.


### OCI-tier package operations {#global-toolchain-oci}

The individual package primitives that manage candidate and `current` symlinks are now grouped under `ocx package`:

<<< @/_scripts/user-guide/oci-primitives.sh{sh}

These are OCI-tier operations — they work on identifiers directly, never read `ocx.toml`.

### Strict isolation — the hard boundary {#global-toolchain-isolation}

The global toolchain is a **shell convenience tier only**. Project builds are hermetic: the project toolchain wins by PATH precedence when you `cd` into a project, and `ocx run` never consults the global file without `--global`.

:::info Why hard isolation, not gap-fill?
[Volta][volta] pioneered this model for Node.js: "Volta covers its tracks … your npm/Yarn scripts never see what's in your toolchain." The alternative — filling in tools the project does not declare from the global set — is exactly what [mise][mise] and [asdf][asdf] do, and it produces the reproducibility hole that OCX is designed to avoid: a collaborator without the same `$OCX_HOME/ocx.toml` gets different resolved tools.
:::

Two commands that are always hermetic regardless of context:

<<< @/_scripts/user-guide/isolation.sh{sh}

A project's `ocx run` cannot resolve a tool that exists only in `$OCX_HOME/ocx.toml`. This is intentional and not a bug — the project declared its dependencies; anything else is ambient noise.

::: tip Learn more
[Command-line reference → root `--global` flag][cmd-add-global] — root flag before the subcommand; affects toolchain-tier commands `add`, `remove`, `lock`, `upgrade`, `pull`, `run`, `env`.
[Env-composition reference → Strict isolation][env-composition-strict-isolation] — reference-level statement of the no-composition rule.
[Command-line reference → `ocx env`][cmd-env-root] — toolchain env exporter, format options, `--shell` safety rule.
:::

## Pin a project's tools {#project}

A repository's contributors and CI runners need the same tool versions — `cmake 3.28`, `shellcheck 0.11`, `goreleaser 2.0` — without arguing over chat or curl-piping installers. The locking mechanisms in the previous section pin a *single* invocation; none of them describe what *the project itself* expects.

A committed `ocx.toml` plus its sibling `ocx.lock` does. The pair makes "the tools this project needs" a piece of source code: reviewable, mergeable, reproducible across machines, resolvable offline once the lock is fetched.

```toml
# ocx.toml
[tools]
cmake      = "ocx.sh/cmake:3.28"
shellcheck = "ocx.sh/shellcheck:0.11"
```

Each value is a fully-qualified [OCI identifier][oci-identifier] — `registry/repo[:tag][@digest]`. Bare-tag forms like `cmake = "3.28"` are rejected so the file is unambiguous regardless of any default-registry config. The schema is published at [`https://ocx.sh/schemas/project/v1.json`][schema-project] and wired through [taplo][taplo] for editor autocompletion.

### Lifecycle commands

<<< @/_scripts/user-guide/project-lifecycle.sh{sh}

[`ocx lock`][cmd-lock] resolves every tag to per-platform leaf digests and writes `ocx.lock`. For each tool, the lock records every platform the publisher ships. Subsequent [`ocx pull`][cmd-pull] / [`ocx run`][cmd-run] runs read the lock for the host platform, never the registry, so two machines on the same commit get the same bytes. The lock carries a hash of the canonicalized `ocx.toml`; if you edit `ocx.toml` and forget to re-run `ocx lock`, dependent commands refuse to run with stale digests.

::: tip Edited `ocx.toml` by hand? Run `ocx lock`.
[`ocx add`][cmd-add] / [`ocx remove`][cmd-remove] regenerate `ocx.lock` for you, but hand-edits to `ocx.toml` do not. The lock carries a hash over the canonicalized `ocx.toml`; commands that read the lock ([`ocx pull`][cmd-pull], [`ocx run`][cmd-run]) detect the drift and exit 65 telling you the lock is stale. Re-run [`ocx lock`][cmd-lock] to sync. The default is intentional: read paths never silently re-resolve, so CI cannot drift behind a stray editor save.

Adding or removing a tool never silently upgrades your other tools — [`ocx add`][cmd-add] and [`ocx remove`][cmd-remove] carry every untouched lock entry forward unchanged. Only [`ocx upgrade`][cmd-upgrade] re-resolves surviving tags.
:::

::: warning Commit your `ocx.lock`
Without it, every contributor and CI runner re-resolves advisory tags against whatever the registry surfaces today. To keep merge conflicts manageable on busy projects, add a [`.gitattributes`][gitattributes] entry that lets `git` union sibling lock entries:

```text
ocx.lock merge=union
```
:::

### Fresh clone {#project-fresh-clone}

Just checked out a repo that already has an `ocx.toml` and `ocx.lock`? Warm the local object store with [`ocx pull`][cmd-pull]:

<<< @/_scripts/user-guide/fresh-clone.sh{sh}

Then run [`direnv allow`][direnv] once to re-evaluate `.envrc`. `ocx direnv export` then puts the locked tools on `PATH`. No re-resolution, no registry writes — the lock is the only input.

### Groups

CI needs `shellcheck` and `shfmt`; a release pipeline needs `goreleaser`; daily development needs neither. Named groups scope subsets so workstations do not download release tooling on first checkout:

```toml
[tools]
cmake = "ocx.sh/cmake:3.28"

[group.ci]
shellcheck = "ocx.sh/shellcheck:0.11"

[group.release]
goreleaser = "ocx.sh/goreleaser:2.0"
```

<<< @/_scripts/user-guide/project-groups.sh{sh}

The same binding name may appear in `[tools]` and any `[group.*]` table — identity is `(group, name)`. This lets a project pin one `shfmt` for daily use and a different one in `ci` without conflict.

### Shell activation

Project tools should land on `PATH` whenever you `cd` into the project — without `eval`-ing on every shell startup. Two entry points, each suited to a different workflow:

- [`ocx direnv export`][cmd-direnv-export] — stateless [direnv][direnv] backend; [`ocx direnv init`][cmd-direnv-init] drops a ready `.envrc` that re-evaluates on each directory entry.
- [`ocx run`][cmd-run] — no hook at all, for CI and scripts: runs a command directly in the locked project env.

The direnv backend exports only — it never installs missing tools or contacts the registry. Run [`ocx pull`][cmd-pull] first.

::: tip Learn more
[Project Toolchain In Depth][in-depth-project] — schema details, declaration-hash canonicalization (RFC 8785 JCS), in-place flock concurrency, per-group binding semantics, multi-project GC retention, SLSA roadmap.
:::

## Run tools from your project {#run}

You have an `ocx.toml`, the lock is current, and you want to invoke a tool from it — without translating binding names into OCI identifiers first. That is what [`ocx run`][cmd-run] is for.

The simplest form runs a command in the default group (`[tools]`) environment:

<<< @/_scripts/user-guide/run-basic.sh{sh}

`--` is mandatory. Every token after `--` is forwarded unchanged to the child. Pass `-g` to scope to a named group:

<<< @/_scripts/user-guide/run-group.sh{sh}

To compose the environment from every group at once, use the `all` keyword:

<<< @/_scripts/user-guide/run-all.sh{sh}

`-g all` expands to `[tools]` + every declared `[group.*]` before env composition. The expansion order determines PATH precedence — groups listed earlier win over later ones (see [Project Toolchain In Depth → Running tools][in-depth-project-running]).

When you only need a specific binding from the composed set, name it:

<<< @/_scripts/user-guide/run-named.sh{sh}

The name must resolve unambiguously in the selected scope; `ocx run` exits 64 if a name is unknown or matches entries in more than one selected group with conflicting identifiers.

:::tip `ocx run` vs `ocx package exec`
`ocx run` is the project-tier command — it reads `ocx.toml` + `ocx.lock` and maps binding names to installed packages. `ocx package exec` is the OCI-tier command — it accepts an OCI identifier directly, with no project file involved.

**Rule:** if you have an `ocx.toml`, use `ocx run`; otherwise use [`ocx package exec`][cmd-package-exec].
:::

::: tip Learn more
[Project Toolchain In Depth → Running tools][in-depth-project-running] — composition order, PATH precedence, exit code table, `all` keyword semantics.
[Environments In Depth][in-depth-environments] — what the composed environment actually contains.
:::

## Use OCX in CI {#ci}

CI environments need tool binaries available with their environment variables exported — but they do not need [version switching][in-depth-storage-symlinks], candidate symlinks, or any of the install-store machinery that supports interactive use.

For **project-toolchain CI**, the recommended flow is:

<<< @/_scripts/user-guide/ci-project.sh{sh}

For **OCI-tier CI** (no `ocx.toml`, direct identifier pinning):

<<< @/_scripts/user-guide/ci-oci.sh{sh}

[`ocx pull`][cmd-pull] (project-tier) and [`ocx package pull`][cmd-package-pull] (OCI-tier) download packages into the [content-addressed package store][in-depth-storage-packages] without creating any symlinks.

To export environment variables into CI runtime files (e.g. `$GITHUB_PATH` / `$GITHUB_ENV` on [GitHub Actions][github-actions-docs]), use `ocx --format json package env` or `ocx --format json env` to get machine-readable output, then write entries to the appropriate CI sink. A dedicated CI export command is a deferred extension point — see the [env-composition reference][env-composition-ref] for the current JSON schema.

::: tip Concurrent matrix builds
`package pull` only touches the package store — no symlinks, no symlink-store mutations. This makes it safe to run concurrently in matrix builds that share a cached [`OCX_HOME`][env-ocx-home]; [content-addressed writes][in-depth-storage-packages] are inherently idempotent.
:::

::: info Relationship to `ocx package install`
[`ocx package install`][cmd-package-install] is `package pull` plus candidate-symlink creation (and optionally `--select` for the current symlink). In CI, the [content-addressed package-store path][in-depth-storage-packages] that `package pull` reports is fully reproducible and digest-derived — symlinks add no value.
:::

::: tip Learn more
[Indices In Depth → Bundled Snapshots][in-depth-indices-bundled] — pair `ocx pull` with a bundled snapshot for end-to-end determinism.
[Storage In Depth → Packages][in-depth-storage-packages] — why concurrent CI writes are safe.
:::

### Cache layers, ephemeral packages {#ci-split-zones}

OCX separates raw OCI content (blobs and extracted layers in [`OCX_CACHE_DIR`][env-ocx-cache-dir]) from assembled package trees ([`OCX_PACKAGES_DIR`][env-ocx-packages-dir]). In CI, these two zones can live on different volumes — useful when blobs and layers are expensive to re-download but assembled packages are cheap to re-assemble from the cached layers.

A typical setup: a persistent cache volume holds the layer store; each job gets a fresh ephemeral volume for its package trees.

```yaml
# GitHub Actions — restore the layer cache, assemble packages fresh each job
- name: Restore layer cache
  uses: actions/cache@v4
  with:
    path: /cache/ocx          # persistent volume: blobs/ + layers/
    key: ocx-layers-${{ runner.os }}-${{ hashFiles('ocx.lock') }}

- name: Install tools
  env:
    OCX_CACHE_DIR: /cache/ocx    # blobs/ and layers/ on the persistent volume
    OCX_PACKAGES_DIR: /tmp/ocx   # assembled packages on an ephemeral volume
  run: ocx pull
```

When `OCX_CACHE_DIR` holds the layers from a previous job, `ocx pull` assembles the packages directly from the cached layers without re-downloading any OCI content. The assembled packages land on `/tmp/ocx` with independent inodes (reflinked or byte-copied, depending on the filesystem) rather than hardlinks, because the two volumes are on different filesystems.

::: tip Learn more
[`OCX_CACHE_DIR` reference][env-ocx-cache-dir] — blobs and layers zone, cross-volume assembly model.
[`OCX_PACKAGES_DIR` reference][env-ocx-packages-dir] — packages zone, ephemeral usage.
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

### Storing credentials {#authentication-storing}

`ocx login REGISTRY` writes credentials to the same `~/.docker/config.json` that `docker login`
and `oras login` use. The three tools interoperate: a credential written by any of them is readable
by the others.

Storage tier (highest priority first):

1. `credHelpers[REGISTRY]` in `~/.docker/config.json` (per-registry helper)
2. `credsStore` (global default helper)
3. Plaintext `auths[REGISTRY].auth` (gated by `--allow-insecure-store`)

For headless CI without a native keychain daemon, pipe the token via `--password-stdin` and pass
`--allow-insecure-store` to opt into the plaintext tier. `OCX_AUTH_*` environment variables still
take precedence over any docker-config-stored credential at read time.

<<< @/_scripts/user-guide/login.sh{sh}

Remove credentials with [`ocx logout`][cmd-logout]. Logout always exits 0, even when the registry
was never logged in — CI cleanup scripts are safe to run unconditionally.

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

On a fresh machine, [`ocx package install cmake:3.28`][cmd-package-install] does not need an explicit `index update` first — when the local snapshot has no entry for the requested tag, OCX resolves it transparently against the registry, persists it, and proceeds with the install.

::: tip Learn more
[Indices In Depth][in-depth-indices] — remote query path, snapshot internals, blob write-through caching, fresh-machine fallback.
[Versioning In Depth → Locking][in-depth-versioning-locking] — why offline snapshots are also a lock.
:::

## Route traffic through a corporate mirror {#mirrors}

In many corporate and air-gapped networks, external registries — `ghcr.io`, `docker.io`, `quay.io`, `ocx.sh` — are firewall-blocked. The organization runs an artifact manager ([JFrog Artifactory][artifactory], [Sonatype Nexus][nexus], [Harbor][harbor]) with proxy/remote repositories that cache those upstreams. OCX needs to route its registry traffic to the mirror without changing the canonical package identity or the content-addressed digest.

The `[mirrors."<upstream-host>"]` config table maps each upstream hostname to its mirror endpoint. OCX appends the upstream repository path after the mirror's repo-key prefix and contacts only the mirror. No origin fallback — in a firewall-controlled network, falling through to the internet is the opposite of intent.

```toml
# ~/.ocx/config.toml  (or $XDG_CONFIG_HOME/ocx/config.toml)
[mirrors."ghcr.io"]
url = "https://company.jfrog.io/ghcr-remote"

[mirrors."docker.io"]
url = "https://company.jfrog.io/dockerhub-remote"
```

With this config, `ocx package install ghcr.io/owner/tool:1.2` fetches the manifest and blobs from `company.jfrog.io/ghcr-remote/owner/tool`. The canonical identifier — `ghcr.io/owner/tool:1.2` — is never changed.

### Relation to the default registry {#mirrors-default-registry}

[`[registry] default`][config-registry-default] and `[mirrors]` are independent and compose. Default injection expands a bare identifier (e.g. `cmake:3.28` → `ocx.sh/cmake:3.28`) at parse time, before any mirror rewrite. If you also configure `[mirrors."ocx.sh"]`, that default-injected identifier is then mirrored. A fully air-gapped setup can mirror every registry the project uses, including the default one.

### Lockfile portability {#mirrors-lockfile}

OCX stores the **canonical upstream host and digest** in `ocx.lock` — never the mirror host. A lock file produced behind a corporate mirror is valid on a machine with direct internet egress, and vice versa. The mirror is a transport detail; the identity of the content is unchanged.

:::info Why the mirror host never appears in the lock
OCX derives every on-disk path — blob store, package store, tag store, symlinks — from the canonical identifier. The mirror only changes which server is contacted; the local object store is keyed the same way with or without a mirror configured. This is what makes the lock portable: `sha256:abc123…` identifies the same bytes regardless of which server served them.
:::

### Unpinned tags and the trust model {#mirrors-trust}

When you install a package with an unpinned tag (e.g. `cmake:3.28`), OCX trusts the mirror's tag→digest resolution the same way it would trust the origin registry's. The mirror could, in principle, map the tag to different content. After resolution, OCX verifies the blob digest against the manifest — a tampered blob is rejected. But the manifest itself came from the mirror's tag resolution.

For tamper-proof installs, pin with [`ocx lock`][cmd-lock]: once a digest is recorded in `ocx.lock`, the tag is never re-resolved and the mirror cannot substitute a different manifest undetected.

Publisher-signature verification (e.g. [cosign][cosign], [Notation][notation]) adds an additional trust layer that validates the publisher's identity independent of the mirror. This is deferred for a post-v1 release.

### Auth for the mirror {#mirrors-auth}

Authenticate against the **mirror** host, not the upstream. Set `OCX_AUTH_<mirror_slug>_*` (replacing non-alphanumeric characters with underscores) or run `ocx login <mirror-host>`. The upstream's credentials are never used on the read path.

```sh
export OCX_AUTH_company_jfrog_io_TYPE=bearer
export OCX_AUTH_company_jfrog_io_TOKEN="<artifactory-token>"
```

### Set mirrors in CI via `OCX_MIRRORS` {#mirrors-env}

For CI or container setups where the command line is not controlled, set mirrors via [`OCX_MIRRORS`][env-mirrors] instead of a config file. The value is a JSON object:

```sh
export OCX_MIRRORS='{"ghcr.io":"https://company.jfrog.io/ghcr-remote"}'
```

`OCX_MIRRORS` wins over `[mirrors]` on a per-host basis and is forwarded to every subprocess `ocx` spawns, so nested invocations — generated launchers, `ocx run` — see the same mirror map automatically.

::: tip Learn more
[Configuration reference → `[mirrors."<host>"]`][config-mirrors] — full schema, auth, interaction table, plain-HTTP note.
[Environment reference → `OCX_MIRRORS`][env-mirrors] — JSON encoding, per-host precedence, subprocess forwarding.
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

## Update OCX {#update-ocx}

OCX is itself an OCX-managed package. The binary lives at `$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin/ocx`. Unlike other packages, `ocx self update` only swaps the `current` symlink — no candidate symlink is created for the new version.

Run [`ocx self update`][cmd-self-update] to update OCX to the latest released version, or [`ocx self update --check`][cmd-self-update] to query for a newer version without installing it.

Both commands bypass the background update-check throttle — they always query the registry. If a new version is available, `ocx self update` installs it and updates the `current` symlink. The `$OCX_HOME/symlinks/…/current/content/bin` PATH entry that `ocx self activate` exports picks up the new binary automatically on the next shell invocation.

When `ocx self update` runs, OCX queries for the latest `major.minor.patch` release tag. Rolling tags (`1`, `1.2`), pre-releases (`1.2.3-rc1`), and build-tagged versions (`1.2.3+build`) are filtered out — the command recommends only stable releases.

The background update-check runs automatically at most once per day (configurable via [`OCX_UPDATE_CHECK_INTERVAL`][env-ocx-update-check-interval]). When a newer version is detected, a notice is printed to stderr at the end of the current command:

```
A new OCX version is available: ocx.sh/ocx/cli:1.1.0. Consider updating by running `ocx self update`.
```

Set [`OCX_NO_UPDATE_CHECK=1`][env-ocx-no-update-check] to disable the background check entirely. The check is also suppressed in CI environments and non-TTY stderr.

When reporting a bug, run [`ocx version --verbose`][cmd-version] to capture commit, build timestamp, target, and CI run URL. For dev-channel builds the output also shows `channel: dev`.

::: tip Learn more
[Command-line reference → `ocx self update`][cmd-self-update] — exit codes, install path, throttle bypass.
[Command-line reference → `ocx version`][cmd-version] — verbose build provenance, JSON schema.
[Environment reference → `OCX_UPDATE_CHECK_INTERVAL`][env-ocx-update-check-interval] — adjust the background check frequency.
:::

## Remove and clean up {#cleanup}

[`ocx package uninstall cmake:3.28`][cmd-package-uninstall] removes the candidate symlink for that tag. The binary stays in the [package store][in-depth-storage-packages] in case other references hold it. Pass `--purge` to also drop the binary if no [other reference][in-depth-storage-gc] remains.

[`ocx clean`][cmd-clean] sweeps the entire store — packages with no live install symlink and no [forward-ref][in-depth-storage-gc] from a dependent package are removed in a single pass, along with any layers and blobs that become unreachable.

When multiple projects share the same `OCX_HOME`, `ocx clean` retains every package referenced by *any* registered project's `ocx.lock` — not just the active one. A project is registered automatically whenever `ocx lock`, `ocx add`, or `ocx remove` writes its lockfile. Deleting a project's directory makes its packages collectable at the next clean (silently — no warning). Browse `$OCX_HOME/projects/` to see which projects are currently registered. Pass `--force` to bypass the project registry; live install symlinks are always honoured.

::: tip Learn more
[Storage In Depth → Garbage Collection][in-depth-storage-gc] — full reachability walk across `refs/symlinks/`, `refs/deps/`, `refs/layers/`, `refs/blobs/`.
[Dependencies In Depth → Garbage Collection][in-depth-dependencies-gc] — why dependencies are protected by dependents, not by back-references.
[Project Toolchain In Depth → Multi-project retention][in-depth-project-multi-project-retention] — symlink ledger, GC semantics, `$OCX_HOME/projects/` browsability.
:::

### Concurrent and CI safety {#cleanup-concurrent-safety}

`ocx clean` is safe to run alongside `ocx install` and other concurrent OCX processes. Before scanning and deleting, it acquires an exclusive advisory lock at `$OCX_STATE_DIR/gc.lock`. Other OCX commands (install, pull) acquire a shared lock on the same file. `clean` waits up to [`OCX_GC_LOCK_TIMEOUT`][env-gc-lock-timeout] seconds (default: 120) for the exclusive lock before giving up with exit code **75** (`TempFail`) — retry-safe.

The lock is per-instance: it lives in `$OCX_STATE_DIR`, which is always per-machine and per-user. Cross-machine or cross-container safety on a shared content volume comes from content-addressing and the mtime grace window below, not from the lock.

**Grace window.** Every object whose directory mtime is younger than [`OCX_GC_GRACE_SECONDS`][env-gc-grace-seconds] (default: 600 seconds) is retained, even if it appears unreachable at scan time. This protects freshly-placed objects that haven't yet been back-referenced — the TOCTOU window between `ocx install` writing a package directory and registering its symlink.

A future mtime (clock skew on a shared volume) is treated as "retain" — the conservative direction. Setting `OCX_GC_GRACE_SECONDS=0` disables the check and collects immediately.

### Shared store for DevContainer fleets {#cleanup-shared-store}

The goal is to share a single content volume across a fleet of DevContainers while still running `ocx clean` from any instance without deleting objects held by another.

When `OCX_SHARED_STORE=true`, `ocx clean` reads the shared-roots ledger at `$OCX_PACKAGES_DIR/roots/`. Each instance writes its live project roots to a per-instance subdirectory: `roots/<instance_id>/<project_hash>`. Before collecting, `ocx clean` unions the pinned digests from every instance directory so no object held by any peer is deleted.

The canonical split for shared-store setups:

```sh
export OCX_PACKAGES_DIR=/vol/shared   # shared across containers
export OCX_STATE_DIR=$HOME/.ocx-local # per-container; holds gc.lock + instance-id
export OCX_SHARED_STORE=true
```

**Safety boundaries.** Shared-roots writes are best-effort and atomic (tempfile + rename). A missed write means the next `clean` on a peer may collect that object; the [grace window](#cleanup-concurrent-safety) provides a time-bounded backstop. Stale ledger directories from departed instances over-retain (safe direction — no spurious deletion). Unreadable peer ledger files (permission error, truncation) cause the cleaner to retain all of that peer's objects rather than guess.

:::info How instance identity works
Each `$OCX_STATE_DIR` contains an `instance-id` file holding a UUID generated on first use. This ID namespaces the shared-roots ledger subdirectory so writes from different containers never collide, even when they share the same packages volume.
:::

::: warning Opt-in only
`OCX_SHARED_STORE` defaults to off. OCX never auto-detects a shared store from the filesystem type. Set it explicitly in every container that participates in the fleet — both when running installs and when running `ocx clean`.
:::

### GC audit log {#cleanup-audit-log}

`ocx clean` appends a JSONL record to `$OCX_STATE_DIR/gc-log.jsonl` for every object it deletes (or would delete in `--dry-run` mode). The log is best-effort — a write failure emits a warning but does not abort the clean.

Each record has the form:

```json
{"schema_version":1,"run_id":"<uuid>","instance_id":"<uuid>","timestamp":"2026-06-14T10:00:00Z","action":"Deleted","object_kind":"Package","path":"/path/to/pkg","digest":"sha256:abc123..."}
```

| Field | Description |
|-------|-------------|
| `schema_version` | Always `1` in this release |
| `run_id` | UUID unique to this `ocx clean` invocation |
| `instance_id` | UUID from `$OCX_STATE_DIR/instance-id` |
| `timestamp` | ISO-8601 UTC |
| `action` | `Deleted` or `WouldDelete` (dry run) |
| `object_kind` | `Package`, `Layer`, `Blob`, or `Temp` |
| `path` | Absolute path of deleted entry |
| `digest` | Content digest, or `null` for `Temp` objects |

When `gc-log.jsonl` reaches [`OCX_GC_LOG_MAX_BYTES`][env-gc-log-max-bytes] (default: 10,485,760 bytes / 10 MiB), it is atomically renamed to `gc-log.jsonl.1` and a fresh log starts. Only one previous-generation file is kept.

To disable audit logging: `export OCX_GC_LOG=off`.

**Forensics.** To see what `ocx clean` deleted in the last run, filter by `run_id`:

```sh
# Most recent clean run (jq required)
last_run=$(tail -1 "$OCX_STATE_DIR/gc-log.jsonl" | jq -r .run_id)
grep "\"$last_run\"" "$OCX_STATE_DIR/gc-log.jsonl" | jq -r '"\(.action) \(.object_kind) \(.path)"'
```

### Network filesystem posture {#cleanup-network-fs}

Advisory locks degrade silently on NFS v2/v3: two concurrent `ocx clean` processes on different machines targeting the same NFS-hosted store will not be serialized by the GC lock. OCX detects when a zone path lives on a network filesystem and warns by default.

Control this with [`OCX_NETWORK_FS`][env-network-fs]:

| Value | What happens |
|-------|-------------|
| `warn` (default) | Log a warning; operation continues. |
| `refuse` | Exit **81** (`PolicyBlocked`) before touching the store. Use in CI to enforce a hard policy. |
| `allow` | Skip the detection check. Use when the probe gives a false positive (e.g. a custom kernel filesystem module). |

For shared DevContainer fleets, the intended pattern is to keep the packages volume on a shared mount but keep `$OCX_STATE_DIR` on a local (per-container) filesystem. This way the GC lock is always local-filesystem-backed and the network posture warning does not fire for state-zone operations.

### Lock-first by default: where are `--locked` and `--frozen`? {#locked-frozen-equivalents}

Users coming from [uv][uv], [Cargo][cargo], or [pnpm][pnpm] often look for `--locked` / `--frozen` flags on read-path commands. OCX folds the *lock-freshness* guarantee into the defaults — read paths refuse stale locks unconditionally, and the only commands that touch `ocx.lock` are explicit mutators — and exposes the *no-new-versions* guarantee as the global [`--frozen`][arg-frozen] flag.

| You used to write… | OCX equivalent |
|---|---|
| [`uv lock --check`][uv-lock] | [`ocx lock --check`][cmd-lock] |
| [`uv sync --locked`][uv-sync] | [`ocx pull`][cmd-pull] / [`ocx run`][cmd-run] (default; exit 65 on drift) |
| [`uv sync --frozen`][uv-sync] | [`ocx --frozen pull`][arg-frozen] / [`ocx --frozen run`][arg-frozen] |
| [`cargo build --locked`][cargo-build] | [`ocx run`][cmd-run] / [`ocx pull`][cmd-pull] (default) |
| [`cargo build --frozen`][cargo-build] | [`--offline`][arg-offline] (subsumes `--frozen`: no unknown tags, no network) |
| [`pnpm install --frozen-lockfile`][pnpm-install] | [`ocx pull`][cmd-pull] (default) |

[`--frozen`][arg-frozen] and [`--offline`][arg-offline] sit on different axes. `--frozen` freezes *version discovery*: a tag already in the local index (or a digest-pinned reference) resolves, but an unknown tag errors instead of being fetched — known content still downloads over the network. `--offline` bans the *network* entirely, so even a digest-pinned blob that is not already cached fails. Use `--frozen` to guarantee no unfamiliar version slips in; use `--offline` for a fully air-gapped run; combine both for the strictest mode (offline wins where they overlap).

Why this asymmetry? OCX is [backend-first][product-context]: read paths refuse stale locks unconditionally so CI scripts cannot silently drift. The mutating commands ([`ocx add`][cmd-add], [`ocx remove`][cmd-remove], [`ocx lock`][cmd-lock], [`ocx upgrade`][cmd-upgrade]) are the only commands that touch `ocx.lock`; if you do not run them, the lock cannot change.

For the "verify a subset would not change without writing" flow, use [`ocx upgrade --check`][cmd-upgrade]. It mirrors [`ocx lock --check`][cmd-lock] but evaluates the partial-resolve candidate against the predecessor.

## Migration {#project-toolchain-migration}

This section covers the changes introduced in the `feat/project-toolchain` release that affect existing workflows.

### Shell integration removed — re-run the installer {#migration-shell-profile}

`ocx shell hook`, `ocx shell init`, `ocx shell env`, and root `ocx install / select / deselect / uninstall / exec` have been removed — they exit 64 if invoked. `ocx ci export` is also removed.

**Global toolchain activation** is now handled by the installer. Re-run the OCX install script to write `$OCX_HOME/env.sh` and the block-marker source line in your login profile:

```sh
# Idempotent — safe to re-run; existing block marker is overwritten in-place.
curl -fsSL https://ocx.sh/install.sh | sh
```

After installation, every new login shell sources `$OCX_HOME/env.sh`, which runs `eval "$(ocx --global env --shell=sh)"`. No `ocx shell init` call is needed — the installer owns profile wiring.

**OCI-tier operations** that moved under `ocx package`:

| Old command | New command |
|---|---|
| `ocx install <pkg>` | `ocx package install <pkg>` |
| `ocx select <pkg>` | `ocx package select <pkg>` |
| `ocx deselect <pkg>` | `ocx package deselect <pkg>` |
| `ocx uninstall <pkg>` | `ocx package uninstall <pkg>` |
| `ocx exec <pkg> -- cmd` | `ocx package exec <pkg> -- cmd` |

For [direnv][direnv]-driven repos, use [`ocx direnv init`][cmd-direnv-init] to write `.envrc` — the project toolchain activation model is unchanged.

::: info Linux + zsh — GUI terminals may not read `.zprofile`
**Known limitation:** On Linux, GUI terminal emulators (GNOME Terminal, Alacritty, Kitty, etc.) typically open non-login shells and do not read `~/.zprofile`. If `ocx` is not found after re-running the installer, add `source ~/.zprofile` (or `. ~/.zprofile`) to your `~/.zshrc`.
:::

### Project mutators are atomic {#migration-atomic-mutators}

`ocx add`, `ocx remove`, and `ocx lock` now acquire an in-place exclusive flock on `ocx.toml` before reading or writing either file. Concurrent invocations from different terminals or parallel CI jobs are serialised. The old `.ocx-lock` sentinel file is gone — remove it from `.gitignore` and run `git rm .ocx-lock` if previously committed.

### `--project` accepts custom filenames {#migration-project-flag}

The `--project` flag and the [`OCX_PROJECT`][env-project] environment variable now accept any path, not just files named `ocx.toml`. The CWD walk still only looks for files named exactly `ocx.toml`.

<!-- external -->
[github-releases-ocx]: https://github.com/ocx-sh/ocx/releases
[artifactory]: https://jfrog.com/artifactory/
[nexus]: https://www.sonatype.com/products/sonatype-nexus-repository
[harbor]: https://goharbor.io/
[cosign]: https://docs.sigstore.dev/cosign/overview/
[notation]: https://notaryproject.dev/
[volta]: https://volta.sh/
[mise]: https://mise.jdx.dev/
[asdf]: https://asdf-vm.com/
[toml]: https://toml.io/
[windows-pathext]: https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/set
[powershell-docs]: https://learn.microsoft.com/en-us/powershell/
[git-bash]: https://git-scm.com/downloads
[batbadbut-advisory]: https://github.com/rust-lang/rust/security/advisories/GHSA-q455-m56c-85mh
[authenticode]: https://learn.microsoft.com/en-us/windows-hardware/drivers/install/authenticode
[signpath]: https://about.signpath.io/
[uv]: https://docs.astral.sh/uv/
[uv-lock]: https://docs.astral.sh/uv/reference/cli/#uv-lock
[uv-sync]: https://docs.astral.sh/uv/reference/cli/#uv-sync
[cargo]: https://doc.rust-lang.org/cargo/
[cargo-build]: https://doc.rust-lang.org/cargo/commands/cargo-build.html
[pnpm]: https://pnpm.io/
[pnpm-install]: https://pnpm.io/cli/install
[product-context]: ./getting-started.md
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[bazel-rules]: https://bazel.build/extending/rules
[devcontainer-features]: https://containers.dev/implementors/features/
[dependabot]: https://docs.github.com/en/code-security/dependabot/working-with-dependabot/keeping-your-actions-up-to-date-with-dependabot
[renovate]: https://docs.renovatebot.com/
[toolchains-llvm]: https://github.com/bazel-contrib/toolchains_llvm/blob/master/toolchain/internal/llvm_distributions.bzl
[oci-identifier]: https://github.com/opencontainers/distribution-spec/blob/main/spec.md#pulling-manifests
[taplo]: https://taplo.tamasfe.dev/
[direnv]: https://direnv.net/
[gitattributes]: https://git-scm.com/docs/gitattributes
[schema-project]: https://ocx.sh/schemas/project/v1.json

<!-- commands -->
[cmd-add-global]: ./reference/command-line.md#global-flag
[cmd-self-activate]: ./reference/command-line.md#self-activate
[cmd-self-setup]: ./reference/command-line.md#self-setup
[cmd-self-update]: ./reference/command-line.md#self-update
[cmd-version]: ./reference/command-line.md#version
[cmd-logout]: ./reference/command-line.md#logout
[cmd-login]: ./reference/command-line.md#login
[cmd-run]: ./reference/command-line.md#run
[arg-config]: ./reference/command-line.md#arg-config
[cmd-clean]: ./reference/command-line.md#clean
[cmd-which]: ./reference/command-line.md#which
[cmd-exec]: ./reference/command-line.md#package-exec
[cmd-launcher-exec]: ./reference/command-line.md#exec
[cmd-package-install]: ./reference/command-line.md#package-install
[cmd-package-select]: ./reference/command-line.md#package-select
[cmd-package-deselect]: ./reference/command-line.md#package-deselect
[cmd-package-uninstall]: ./reference/command-line.md#package-uninstall
[cmd-package-exec]: ./reference/command-line.md#package-exec
[cmd-package-env]: ./reference/command-line.md#package-env
[cmd-install]: ./reference/command-line.md#package-install
[cmd-select]: ./reference/command-line.md#package-select
[cmd-deselect]: ./reference/command-line.md#package-deselect
[cmd-uninstall]: ./reference/command-line.md#package-uninstall
[cmd-index-update]: ./reference/command-line.md#index-update
[cmd-package-pull]: ./reference/command-line.md#package-pull
[cmd-deps]: ./reference/command-line.md#deps
[cmd-env]: ./reference/command-line.md#env
[cmd-env-root]: ./reference/command-line.md#env-root
[cmd-add]: ./reference/command-line.md#add
[cmd-remove]: ./reference/command-line.md#remove
[cmd-lock]: ./reference/command-line.md#lock
[cmd-upgrade]: ./reference/command-line.md#upgrade
[cmd-pull]: ./reference/command-line.md#pull
[cmd-direnv-export]: ./reference/command-line.md#direnv-export
[cmd-direnv-init]: ./reference/command-line.md#direnv-init
[arg-remote]: ./reference/command-line.md#arg-remote
[arg-offline]: ./reference/command-line.md#arg-offline
[arg-frozen]: ./reference/command-line.md#arg-frozen
[arg-index]: ./reference/command-line.md#arg-index

<!-- environment -->
[env-ocx-no-completions]: ./reference/environment.md#ocx-no-completions
[env-ocx-binary-pin]: ./reference/environment.md#ocx-binary-pin
[env-ocx-home]: ./reference/environment.md#ocx-home
[env-ocx-index]: ./reference/environment.md#ocx-index
[env-ocx-cache-dir]: ./reference/environment.md#ocx-cache-dir
[env-ocx-packages-dir]: ./reference/environment.md#ocx-packages-dir
[env-config]: ./reference/environment.md#ocx-config
[env-no-config]: ./reference/environment.md#ocx-no-config
[env-no-modify-path]: ./reference/environment.md#ocx-no-modify-path
[env-project]: ./reference/environment.md#ocx-project
[env-auth-type]: ./reference/environment.md#ocx-auth-registry-type
[env-auth-user]: ./reference/environment.md#ocx-auth-registry-user
[env-auth-token]: ./reference/environment.md#ocx-auth-registry-token
[env-docker-config]: ./reference/environment.md#external-docker-config
[env-ocx-no-update-check]: ./reference/environment.md#ocx-no-update-check
[env-ocx-update-check-interval]: ./reference/environment.md#ocx-update-check-interval
[env-gc-lock-timeout]: ./reference/environment.md#ocx-gc-lock-timeout
[env-gc-grace-seconds]: ./reference/environment.md#ocx-gc-grace-seconds
[env-gc-log-max-bytes]: ./reference/environment.md#ocx-gc-log-max-bytes
[env-network-fs]: ./reference/environment.md#ocx-network-fs
[env-shell-activation-files]: ./reference/environment.md#shell-activation-files
[xdg-basedir]: ./reference/environment.md#external-xdg-config-home
[env-ref]: ./reference/environment.md

<!-- reference pages -->
[config-ref]: ./reference/configuration.md
[config-registry-default]: ./reference/configuration.md#keys-registry-default
[config-mirrors]: ./reference/configuration.md#keys-mirrors
[env-mirrors]: ./reference/environment.md#ocx-mirrors
[env-composition-strict-isolation]: ./reference/env-composition.md#strict-isolation
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
[in-depth-project]: ./in-depth/project.md
[in-depth-project-multi-project-retention]: ./in-depth/project.md#multi-project-retention
[in-depth-project-running]: ./in-depth/project.md#running
