---
outline: deep
---
# CI Integration {#ci-integration}

Running OCX in a CI pipeline has one core challenge: shell environment changes do not cross step boundaries.

When a pipeline runs `eval "$(ocx env --shell=bash)"`, those exports live only in the current shell. The next step starts a fresh process — `PATH` is reset, variables are gone, and any tool installed in step one is invisible in step two.

`ocx env --ci` and `ocx package env --ci` solve this by writing the composed environment into the CI system's own persistence channel instead of printing shell export lines. The runner picks up that channel between steps, so later steps see the full tool environment without any extra glue code.

:::info `--shell` vs `--ci` — which one to use
`--shell` emits eval-safe export lines for the **current step only**. Use it inside a single step that sources the env and immediately runs commands. `--ci` writes to the runner's persistence channel so the env is available to **every subsequent step**. Use it whenever tools installed in one step must be reachable in later steps.
:::

## GitHub Actions {#ci-github-actions}

[GitHub Actions][github-actions-docs] runners provide two file-based channels for sharing state across steps:

- [`$GITHUB_PATH`][github-actions-set-path] — each appended line is **prepended** to `PATH` for all later steps. OCX writes `PATH` entries here, so OCX-installed tools land leftmost (highest priority) in `PATH` regardless of when in the job `ocx env --ci=github` runs.
- [`$GITHUB_ENV`][github-actions-set-env] — each `KEY=VALUE` line (or heredoc block for multiline values) is exported to all later steps.

`ocx env --ci=github` reads the paths of those files from the runner's own `GITHUB_PATH` and `GITHUB_ENV` variables and appends the resolved tool paths and variables directly. No `jq`, no redirect.

Only the literal `PATH` variable goes to `$GITHUB_PATH`. All other path-type variables — `LD_LIBRARY_PATH`, `MANPATH`, `PKG_CONFIG_PATH`, and any others declared in package metadata — are written to `$GITHUB_ENV` as `KEY=value`, with OCX-provided directories prepended to the existing value.

Running `--ci=github` outside a [GitHub Actions][github-actions-docs] runner — where `GITHUB_ENV` and `GITHUB_PATH` are unset — exits 78 (configuration error).

### Toolchain-tier example {#ci-github-actions-toolchain}

The turnkey path is the [`ocx-sh/setup-ocx`][setup-ocx] action. In project mode it installs OCX, pulls the project toolchain from `ocx.lock`, and replays `ocx env --ci=github` into `$GITHUB_PATH` / `$GITHUB_ENV` — all in one step, with a build cache keyed on `sha256(ocx.lock)`. Pin it by commit SHA (with a human-readable version comment), the same way [GitHub recommends pinning third-party actions][github-actions-pin]:

```yaml
jobs:
  build:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      - uses: ocx-sh/setup-ocx@<sha> # v1.2.2

      - name: Build
        run: cmake --version && ninja --version
```

After the [`setup-ocx`][setup-ocx] step, every subsequent step sees the project's resolved tool directories in `PATH` and any declared environment variables.

::: details Without the action — install OCX manually
If you prefer not to depend on the [`setup-ocx`][setup-ocx] action, install OCX with the [POSIX installer][setup-ocx-sh] and replay the toolchain yourself. The installer puts the `ocx` binary under `~/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin` — add that to `$GITHUB_PATH` so `ocx` resolves in later steps:

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
        run: cmake --version && ninja --version
```
:::

### OCI-tier example {#ci-github-actions-oci}

For individual OCI packages rather than a full project toolchain, there is no `ocx.lock` to pull, so install OCX directly — either with the [`ocx-sh/setup-ocx`][setup-ocx] action (install only) or the [POSIX installer][setup-ocx-sh] — and resolve each package with `ocx package env`:

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

      - name: Resolve tool environment
        run: ocx package env --ci=github node:20 python:3.12

      - name: Run tests
        run: node --version && python --version
```

:::tip Auto-detection
Bare `--ci` without `=github` also works inside [GitHub Actions][github-actions-docs] because the runner sets `GITHUB_ACTIONS=true`. Either form is valid; `--ci=github` is explicit and recommended for readability.
:::

## GitLab CI/CD {#ci-gitlab}

`--ci=gitlab` targets the [GitLab step runner][gitlab-step-runner-docs] (`run:` keyword), which is an experimental feature available in self-managed instances running the GitLab [step runner][gitlab-step-runner-docs]. It is **not** compatible with traditional `script:` jobs.

The step runner persists variables across steps within a job using an export file at `${{ export_file }}`. Each entry in that file is a JSON object on its own line:

```json
{"name":"KEY","value":"VALUE"}
```

`ocx env --ci=gitlab` produces this exact format — either to `--export-file=PATH` or to stdout when `--export-file` is omitted.

[GitLab CI/CD][gitlab-ci-docs] has no separate `PATH` channel. `ocx env --ci=gitlab` flattens all path-type entries: package values are prepended to the current process value of `PATH` (and any other path-type variable such as `LD_LIBRARY_PATH`), joined with the platform path separator (`:` on Unix, `;` on Windows), and emitted as a single JSON-lines entry. The step runner injects the resulting value into the environment of subsequent steps.

:::warning GitLab Functions / step runner only
`--ci=gitlab` produces JSON-lines output (`{"name":"…","value":"…"}`). This format is consumed by the [GitLab step runner][gitlab-step-runner-docs] via `${{ export_file }}` — an **experimental** feature in GitLab, available on self-managed instances running the step runner, with the `run:` keyword only.

`${{ export_file }}` persists environment to **later steps within the same job**. It does not propagate across separate jobs by itself.

Traditional `script:` jobs cannot consume this JSON format at all. For cross-job variable passing in traditional `script:` jobs, [GitLab CI/CD][gitlab-ci-docs] provides [`artifacts: reports: dotenv`][gitlab-ci-dotenv], which requires bare `KEY=VALUE` lines — not JSON. OCX does not currently emit dotenv format directly, so cross-job variable passing in traditional pipelines requires a separate solution.
:::

### Toolchain-tier example {#ci-gitlab-toolchain}

This example uses the GitLab step runner's `run:` keyword. Install OCX and set up the toolchain in two steps of the same job, then use the tools in a third step of the same job:

```yaml
build:
  run:
    - name: Install OCX
      script: |
        curl -fsSL https://setup.ocx.sh/sh | sh
        export PATH="$HOME/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin:$PATH"
    - name: Set up toolchain
      script: ocx env --ci=gitlab --export-file="${{ export_file }}"
    - name: Build
      script: |
        cmake --version
        ninja --version
```

`${{ export_file }}` is a runner-provided path, not a user-defined variable. The step runner reads the JSON-lines written there and injects each entry into the environment of later steps within the same job. The "Build" step sees the full tool environment because it follows "Set up toolchain" in the same job.

### Redirect to stdout {#ci-gitlab-stdout}

When `--export-file` is omitted, output goes to stdout. Redirect it to wherever the step runner expects:

```yaml
- name: Set up toolchain
  script: ocx env --ci=gitlab >> "${{ export_file }}"
```

The `>>` append-redirect is only needed when not using `--export-file`. With `--export-file`, OCX opens the file directly in append mode — no redirect required.

### OCI-tier example {#ci-gitlab-oci}

For individual OCI packages rather than a full project toolchain:

```yaml
test:
  run:
    - name: Install OCX
      script: |
        curl -fsSL https://setup.ocx.sh/sh | sh
        export PATH="$HOME/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin:$PATH"
    - name: Resolve tool environment
      script: ocx package env --ci=gitlab --export-file="${{ export_file }}" node:20 python:3.12
    - name: Run tests
      script: |
        node --version
        python --version
```

:::tip Auto-detection
Bare `--ci` without `=gitlab` also works inside [GitLab CI/CD][gitlab-ci-docs] because the runner sets `GITLAB_CI=true`.
:::

<!-- external -->
[setup-ocx]: https://github.com/ocx-sh/setup-ocx
[setup-ocx-sh]: https://setup.ocx.sh/sh
[github-actions-pin]: https://docs.github.com/en/actions/security-for-github-actions/security-guides/security-hardening-for-github-actions#using-third-party-actions
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[github-actions-set-env]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/workflow-commands-for-github-actions#setting-an-environment-variable
[github-actions-set-path]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/workflow-commands-for-github-actions#adding-a-system-path
[gitlab-ci-docs]: https://docs.gitlab.com/ee/ci/
[gitlab-step-runner-docs]: https://docs.gitlab.com/ci/functions/create/
[gitlab-ci-dotenv]: https://docs.gitlab.com/ee/ci/yaml/artifacts_reports.html#artifactsreportsdotenv

<!-- environment -->
[env-github-actions]: ../reference/environment.md#external-github-actions
[env-github-env]: ../reference/environment.md#external-github-env
[env-github-path]: ../reference/environment.md#external-github-path
[env-gitlab-ci]: ../reference/environment.md#external-gitlab-ci
