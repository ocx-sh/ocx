---
title: "Use ocx in GitLab CI"
description: "Put a pinned ocx toolchain on PATH in a GitLab CI job, with the classic script runner or the step runner."
---
<!-- doc_type: how-to -->
<!-- doc_tier: integration -->

This page shows how to make an ocx toolchain available in a GitLab CI/CD job. It covers traditional `script:` jobs, which most pipelines use, and the experimental step runner's export file.

GitLab has two ways to give a job its tools. A `script:` job runs every line in one shell, so a single `eval` reaches the lines after it. The step runner starts a fresh process per step and reads an export file between them.

Pick the route by the runner your job uses. The two do not mix: `ocx env --ci=gitlab` writes JSON lines that a `script:` job cannot read.

## Traditional script jobs {#ci-gitlab-script}

GitLab runs `before_script` and `script` of one job in a single shell session. Environment exported on one line is visible on the next. That makes [`ocx env --shell`][cmd-env-root] the right tool, and it needs no CI-specific flag.

```yaml
build:
  image: ubuntu:24.04
  script:
    - curl -fsSL https://setup.ocx.sh/sh | sh
    - export PATH="$HOME/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin:$PATH"
    - ocx pull
    - eval "$(ocx env --shell=sh)"
    - cmake --version
    - shellcheck --version
```

[`ocx pull`][cmd-pull] reads `ocx.toml` and `ocx.lock` from the repository, so the job needs the project files and nothing else. The `eval` line puts the composed environment on the rest of the job. For a single command, [`ocx exec`][cmd-exec] runs it inside the composed environment without touching the shell:

```yaml
build:
  script:
    - ocx exec -- cmake --version
```

The environment stops at the end of the job. A later job repeats the install and the `eval`, and the cache below keeps that cheap.

### Cache the package store {#ci-gitlab-cache}

A job that downloads every package on each run is slow and depends on the registry. Point `OCX_HOME` inside the project directory, because GitLab caches only paths under it. Key the cache on `ocx.lock`, so the cache changes exactly when the toolchain does.

```yaml
variables:
  OCX_HOME: $CI_PROJECT_DIR/.ocx-home

cache:
  key:
    files:
      - ocx.lock
  paths:
    - .ocx-home/blobs
    - .ocx-home/layers
    - .ocx-home/packages
    - .ocx-home/index
```

[`OCX_HOME`][env-ocx-home] is the data root, so the installer and every later `ocx` command use the same directory. Add `$OCX_HOME/symlinks/ocx.sh/ocx/cli/current/content/bin` to `PATH` instead of the `$HOME` path shown above.

## Step runner jobs {#ci-gitlab}

`--ci=gitlab` targets the [GitLab step runner][gitlab-step-runner-docs] (`run:` keyword). It is an experimental feature, available on self-managed instances that run the step runner. It is **not** compatible with traditional `script:` jobs.

The step runner persists variables across steps within a job using an export file at `${{ export_file }}`. Each entry in that file is a JSON object on its own line:

```json
{"name":"KEY","value":"VALUE"}
```

`ocx env --ci=gitlab` produces this exact format. It writes to `--export-file=PATH`, or to stdout when `--export-file` is omitted.

[GitLab CI/CD][gitlab-ci-docs] has no separate `PATH` channel. `ocx env --ci=gitlab` flattens all path-type entries. Package values are prepended to the current process value of `PATH`, and of any other path-type variable such as `LD_LIBRARY_PATH`. They are joined with the platform separator (`:` on Unix, `;` on Windows) and emitted as one JSON-lines entry.

A [`list`][reference-env-list]-type variable flattens the same way but folds the opposite direction. Package values are *appended* to the current process value, joined by the key's own separator, and emitted as their own entry. The step runner injects the resulting values into the environment of later steps.

:::caution[Step runner only]
`--ci=gitlab` produces JSON lines that only the [GitLab step runner][gitlab-step-runner-docs] consumes. A traditional `script:` job cannot read the format. `${{ export_file }}` persists environment to **later steps of the same job**. It does not cross jobs.

For passing variables between jobs in a traditional pipeline, GitLab offers [`artifacts: reports: dotenv`][gitlab-ci-dotenv]. That needs bare `KEY=VALUE` lines, which OCX does not emit. Repeat the `eval` in each job instead.
:::

### Toolchain-tier example {#ci-gitlab-toolchain}

The example needs `ocx.toml` and `ocx.lock` committed to the repository, with `kitware/cmake` and `shellcheck` declared as tools. [Your first project][guide-first-project] creates both files.

Install OCX and set up the toolchain in two steps of the same job. A third step of the same job then uses the binaries:

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
        shellcheck --version
```

`${{ export_file }}` is a runner-provided path, not a variable you define. The runner reads the JSON lines written there and injects each entry into later steps of the same job. The Build step sees the full environment because it follows the setup step.

The JSON line freezes the `PATH` of that moment, ambient entries included. A later step on a different image sees the paths of the first one.

### Redirect to stdout {#ci-gitlab-stdout}

When `--export-file` is omitted, output goes to stdout. Redirect it to the file the step runner expects:

```yaml
- name: Set up toolchain
  script: ocx env --ci=gitlab >> "${{ export_file }}"
```

The `>>` append is only needed without `--export-file`. With the option, OCX opens the file in append mode itself.

### OCI-tier example {#ci-gitlab-oci}

For individual OCI packages rather than a project toolchain:

```yaml
test:
  run:
    - name: Install OCX
      script: |
        curl -fsSL https://setup.ocx.sh/sh | sh
        export PATH="$HOME/.ocx/symlinks/ocx.sh/ocx/cli/current/content/bin:$PATH"
    - name: Resolve package environment
      script: ocx package env --ci=gitlab --export-file="${{ export_file }}" ocx.sh/kitware/cmake:3.28 ocx.sh/shellcheck/shellcheck:0.11
    - name: Test
      script: |
        cmake --version
        shellcheck --version
```

:::tip[Auto-detection]
Bare `--ci` without `=gitlab` also works inside [GitLab CI/CD][gitlab-ci-docs], because the runner sets `GITLAB_CI=true`. `--ci` cannot combine with `--shell`: clap rejects the pair with exit 64.
:::

## Go further {#ci-next}

- [Run ocx unattended][p-unattended-ci] lists the variables to set and the runner variables ocx reads, including the GitLab ones.
- [Registry authentication recipes][p-registry-auth-recipes] cover private registries.
- [Use ocx in GitHub Actions][p-github-actions] covers the other provider and the shared pull flow.

<!-- external -->
[gitlab-ci-docs]: https://docs.gitlab.com/ee/ci/
[gitlab-step-runner-docs]: https://docs.gitlab.com/ci/functions/create/
[gitlab-ci-dotenv]: https://docs.gitlab.com/ee/ci/yaml/artifacts_reports.html#artifactsreportsdotenv

<!-- commands -->
[cmd-env-root]: ../reference/command-line/commands.md#env-root
[cmd-exec]: ../reference/command-line/commands.md#exec
[cmd-pull]: ../reference/command-line/commands.md#pull

<!-- environment -->
[env-ocx-home]: ../reference/environment.md#ocx-home

<!-- reference -->
[reference-env-list]: ../reference/metadata/env.md#env-list

<!-- pages -->
[p-unattended-ci]: ./unattended-ci.md
[p-registry-auth-recipes]: ./registry-auth-recipes.md
[p-github-actions]: ./github-actions.md
[guide-first-project]: ../getting-started/first-project.mdx
