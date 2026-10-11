---
title: "Use ocx in GitHub Actions"
description: "Put a pinned ocx toolchain on PATH for every step of a GitHub Actions job."
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

This page shows how to make an ocx toolchain available to every step of a GitHub Actions job. It covers the `setup-ocx` action, the plain `ocx env --ci` route, and the pull flow for runners that share a cache.

Shell environment changes do not cross step boundaries. An `eval "$(ocx env --shell=bash)"` lives only in the current shell, so a package set up in step one is invisible in step two.

`ocx env --ci` and `ocx package env --ci` write the composed environment into the CI system's own persistence channel instead of printing export lines. Later steps see the full environment without glue code.

:::note[`--shell` vs `--ci`: which one to use]
`--shell` emits eval-safe export lines for the **current step only**. Use it inside one step that sources the environment and runs commands at once. `--ci` writes to the runner's persistence channel, so the environment reaches **every later step**. Use it whenever a package set up in one step must be reachable in the next.
:::

## Persist the environment across steps {#ci-github-actions}

[GitHub Actions][github-actions-docs] runners provide two file-based channels for sharing state across steps:

- [`$GITHUB_PATH`][github-actions-set-path]: each appended line is **prepended** to `PATH` for all later steps. OCX writes `PATH` entries here, so OCX-composed package directories land leftmost, which is the highest priority.
- [`$GITHUB_ENV`][github-actions-set-env]: each `KEY=VALUE` line (or heredoc block for multiline values) is exported to all later steps.

`ocx env --ci=github` reads the paths of those files from the runner's own `GITHUB_PATH` and `GITHUB_ENV` variables. It appends the resolved package paths and variables directly, with no `jq` and no redirect. The command is silent on success, so the files are the only evidence.

Only the literal `PATH` variable goes to `$GITHUB_PATH`. Every other path-type variable goes to `$GITHUB_ENV` as `KEY=value`, with OCX-provided directories prepended to the existing value. Examples are `LD_LIBRARY_PATH`, `MANPATH` and `PKG_CONFIG_PATH`.

A [`list`][reference-env-list]-type variable such as `JDK_JAVA_OPTIONS` also goes to `$GITHUB_ENV`. It folds the opposite way: OCX's contribution is *appended* to the existing value, joined by the key's separator.

Outside a GitHub Actions runner, `GITHUB_ENV` and `GITHUB_PATH` are unset and `--ci=github` exits 78 (configuration error). Bare `--ci` with no provider and no detected CI exits 64.

### Toolchain-tier example {#ci-github-actions-toolchain}

The turnkey path is the [`ocx-sh/setup-ocx`][setup-ocx] action. In project mode it installs OCX and pulls the project toolchain from `ocx.lock`. It then replays `ocx env --ci=github` into `$GITHUB_PATH` and `$GITHUB_ENV`, with a build cache keyed on the hash of `ocx.lock`.

The example needs `ocx.toml` and `ocx.lock` committed to the repository, with `kitware/cmake` and `shellcheck` declared as tools. [Your first project][guide-first-project] creates both files.

```yaml
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: ocx-sh/setup-ocx@v1

      - name: Build
        run: cmake --version && shellcheck --version
```

After the [`setup-ocx`][setup-ocx] step, every later step sees the project's resolved package directories in `PATH` and any declared environment variables.

The example above uses the `v1` tag for readability. [GitHub recommends pinning third-party actions by commit SHA][github-actions-pin]. Resolve the SHA of a release tag yourself and keep the tag as a comment:

```sh
git ls-remote https://github.com/ocx-sh/setup-ocx 'refs/tags/v1*'
```

The line below pins `v1.4.0`, the release current when this page was written.

```yaml
      - uses: ocx-sh/setup-ocx@25fa771f8572572dc64528db89560de68a163a0e # v1.4.0
```

<details><summary>Without the action: install OCX manually</summary>

To avoid the [`setup-ocx`][setup-ocx] action, install OCX with the [POSIX installer][setup-ocx-sh] and replay the toolchain yourself. The installer puts the `ocx` binary under `~/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin`. Add that directory to `$GITHUB_PATH` so `ocx` resolves in later steps.

For a reproducible runner, install a pinned version. The [version-pinned installer URL][p-l-ocxx-install-bare-binary-pin] and [`ocx self setup <tag>`][p-ted-install-ocxx-manual-pinned] both give every run the same build.

```yaml
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install OCX
        run: |
          curl -fsSL https://setup.ocx.sh/sh | sh
          echo "$HOME/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin" >> "$GITHUB_PATH"

      - name: Set up toolchain
        run: ocx env --ci=github

      - name: Build
        run: cmake --version && shellcheck --version
```

</details>

### OCI-tier example {#ci-github-actions-oci}

Individual OCI packages have no `ocx.lock` to pull. Install OCX with the action (install only) or the [POSIX installer][setup-ocx-sh], then resolve each package with `ocx package env`:

```yaml
jobs:
  test:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - name: Install OCX
        run: |
          curl -fsSL https://setup.ocx.sh/sh | sh
          echo "$HOME/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin" >> "$GITHUB_PATH"

      - name: Resolve package environment
        run: ocx package env --ci=github ocx.sh/kitware/cmake:3.28 ocx.sh/shellcheck/shellcheck:0.11

      - name: Test
        run: cmake --version && shellcheck --version
```

An identifier needs its namespace. A bare `node:20` fails with exit 79 because the name is not in the index.

:::tip[Auto-detection]
Bare `--ci` without `=github` also works inside [GitHub Actions][github-actions-docs], because the runner sets `GITHUB_ACTIONS=true`. `--ci=github` is explicit and reads better in a workflow file.
:::

## Pull the toolchain in CI {#ci}

CI needs package binaries available with their environment variables exported. It does not need [version switching][in-depth-storage-symlinks], candidate symlinks or the rest of the install-store machinery that supports interactive use.

For **project-toolchain CI**, with `ocx.toml` and `ocx.lock` committed (see [your first project][guide-first-project]), the recommended flow is:

<<< @/_scripts/user-guide/ci-project.sh{sh}

`ocx lock --check` fails the job when the lock is stale, and `ocx --frozen pull` downloads only the locked digests. [Freeze what a job resolves][p-unattended-ci-frozen] explains both switches.

For **OCI-tier CI**, with no `ocx.toml` and direct identifier pinning:

<<< @/_scripts/user-guide/ci-oci.sh{sh}

[`ocx pull`][cmd-pull] (project tier) and [`ocx package pull`][cmd-package-pull] (OCI tier) download packages into the [content-addressed package store][in-depth-storage-packages] without creating any symlinks. To export the result into the runner's files, use [`ocx env --ci=github`](#ci-github-actions) as shown above.

:::tip[Concurrent matrix builds]
`package pull` only touches the package store. It makes no symlink-store mutations, so it is safe in matrix builds that share a cached [`OCX_HOME`][env-ocx-home]. [Content-addressed writes][in-depth-storage-packages] are idempotent.
:::

:::note[Relationship to `ocx package install`]
[`ocx package install`][cmd-package-install] is `package pull` plus candidate-symlink creation, and optionally `--select` for the `current` symlink. In CI the digest-derived store path that `package pull` reports is already reproducible, so symlinks add no value.
:::

### Cap parallel pulls {#ci-jobs}

[`OCX_JOBS`][env-ocx-jobs] caps how many root packages are pulled in parallel. It applies to every command that fans out through the package manager: `install`, `pull`, `package pull`, `exec` and `env`.

`0` means all logical cores, matching [GNU `parallel -j 0`][gnu-parallel-j0]. It differs from Cargo's `--jobs 0`, which errors. A negative or non-numeric value is ignored with a warning, and unset is unbounded. The [`--jobs`][arg-jobs] option takes precedence over the variable.

```sh
export OCX_JOBS=4
```

## Carry PATH to the next step {#global-toolchain-ci-path}

A step's environment does not survive to the next step. Exporting `PATH` in one `run:` block and expecting the next block to see it is the usual way a working toolchain looks broken on a runner.

[`ocx env --ci=github`](#ci-github-actions) solves this. When you want only the launcher directory on `PATH`, append it to `$GITHUB_PATH` yourself. This recipe needs `jq` on the runner, and `ocx` on `PATH` from an earlier install or [`setup-ocx`][setup-ocx] step. Render first, then append:

```yaml
- run: ocx pull
- run: ocx --format json shell state | jq -r .toolchain_bin >> "$GITHUB_PATH"
- run: cmake --version        # a later step: the toolchain is on PATH
```

The order is the whole recipe. [`ocx pull`][cmd-pull] writes the launchers and the render stamp, and the append then publishes a directory that has something in it. Reversed, the append publishes an empty directory and the third step cannot find a launcher that was never rendered.

`$GITHUB_PATH` accepts any string and never checks it. A wrong directory costs a green step and a failure in the next job. That is why the recipe reads the `toolchain_bin` field instead of joining a path: `.toolchain_home + "/bin"` looks right, appends cleanly and names nothing.

## Go further {#ci-next}

- [Run ocx unattended][p-unattended-ci] covers the variables a pipeline should set and the runner variables ocx reads.
- [Registry authentication recipes][p-registry-auth-recipes] cover GHCR and ECR.
- [Publisher CI][p-publisher-ci] covers signing, SBOMs and promotion from a workflow.
- [Use ocx in GitLab CI][p-gitlab-ci] covers the other provider.

<!-- external -->
[setup-ocx]: https://github.com/ocx-sh/setup-ocx
[setup-ocx-sh]: https://setup.ocx.sh/sh
[github-actions-pin]: https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions#using-third-party-actions
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[github-actions-set-env]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/workflow-commands-for-github-actions#setting-an-environment-variable
[github-actions-set-path]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/workflow-commands-for-github-actions#adding-a-system-path
[gnu-parallel-j0]: https://www.gnu.org/software/parallel/parallel.html

<!-- commands -->
[cmd-pull]: ../reference/command-line/commands.md#pull
[cmd-package-pull]: ../reference/command-line/package-consuming.md#package-pull
[cmd-package-install]: ../reference/command-line/package-consuming.md#package-install

<!-- arguments -->
[arg-jobs]: ../reference/command-line/index.md#arg-jobs

<!-- environment -->
[env-ocx-home]: ../reference/environment.md#ocx-home
[env-ocx-jobs]: ../reference/environment.md#ocx-jobs

<!-- reference -->
[reference-env-list]: ../reference/metadata/env.md#env-list

<!-- in depth -->
[in-depth-storage-symlinks]: ../explanation/storage.mdx#symlinks
[in-depth-storage-packages]: ../explanation/storage.mdx#packages

<!-- pages -->
[p-l-ocxx-install-bare-binary-pin]: ../getting-started/install-ocx.mdx#script-options
[p-ted-install-ocxx-manual-pinned]: ../getting-started/install-ocx.mdx#manual-pinned
[p-unattended-ci]: ./unattended-ci.md
[p-unattended-ci-frozen]: ./unattended-ci.md#unattended-frozen
[guide-first-project]: ../getting-started/first-project.mdx
[p-registry-auth-recipes]: ./registry-auth-recipes.md
[p-publisher-ci]: ./publisher-ci.md
[p-gitlab-ci]: ./gitlab-ci.md
