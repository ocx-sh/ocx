---
layout: doc
outline: deep
---
# mirror.yml Reference

`mirror.yml` describes one tool to mirror — where to fetch upstream releases, which platforms to build for, how to test each bundle, and how to report results. The file is consumed by `ocx-mirror sync`, `ocx-mirror check`, and all `ocx-mirror pipeline` subcommands.

## Top-level keys {#top-level}

| Key | Type | Required | Purpose |
|-----|------|----------|---------|
| `name` | string | Yes | Tool name, used in log output and notify messages |
| `target` | object | Yes | OCI registry and repository to push to |
| `source` | object | Yes | Upstream release source ([GitHub Releases][github-releases] or URL index) |
| `assets` | object | Yes | Platform → regex list mapping for selecting upstream release archives |
| `asset_type` | string | No | `Archive` (default) or `Binary` |
| `cascade` | boolean | No | Cascade rolling tags on push (`false` by default) |
| `versions` | object | No | Version filter (min/max bounds, `new_per_run`, backfill order) |
| `verify` | object | No | Checksum verification options |
| `concurrency` | object | No | Parallel download and push limits |
| `tests` | array | No* | Commands to run against each installed bundle. Required when `pipeline generate ci` is used. |
| `platforms` | object | No* | GHA runner and container matrix. Required when `pipeline generate ci` is used. |
| `ocx_mirror` | object | No* | ocx-mirror version pin for generated workflows. Required when any Linux platform declares containers. |
| `notify` | object | No | Discord webhook notification settings |

The `tests`, `platforms`, `ocx_mirror`, and `notify` keys are used only by `ocx-mirror pipeline` subcommands. `sync` and `check` ignore them.

## `tests` {#tests}

Declares the smoke-test commands to run against each installed bundle. Every entry runs for every `(version, platform, container)` combination in the matrix.

```yaml
tests:
  - name: version
    command: cmake --version
  - name: smoke
    command: bash ./tests/smoke.sh
```

**Rules:**

- Required: must contain at least one entry when used with `pipeline generate ci`.
- `name` must be unique within the file and must match `^[a-zA-Z][a-zA-Z0-9_-]*$`. The name appears as the JUnit test-case name, so it must be stable across runs.
- `command` is a single-line string. Multi-line scripts must be files in the mirror repository and invoked via shell (`bash ./tests/smoke.sh`, `pwsh -File ./tests/smoke.ps1`).
- No `script` field or auto-detection — command-only by design.

**Environment exposed to every test command:**

| Variable | Value |
|----------|-------|
| `OCX_INSTALL_DIR` | Path where `ocx package test` materialized the package |
| `OCX_VERSION` | Mirrored version string (e.g., `3.29.0`) |
| `OCX_PLATFORM` | Platform slug (e.g., `linux/amd64`) |
| `OCX_IMAGE` | Container image; empty on native legs |
| `OCX_TEST_NAME` | The `tests[].name` value for this invocation |

## `platforms` {#platforms}

Declares the GHA runner and container matrix for the generated workflow. Each key is a platform slug in `<os>/<arch>` form.

```yaml
platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - { image: "ubuntu:24.04", shell: bash }
      - { image: "alpine:3.20",  shell: sh }
      - { image: "fedora:40",    shell: bash }

  linux/arm64:
    runner: ubuntu-24.04-arm
    containers:
      - { image: "ubuntu:24.04", shell: bash }
      - { image: "alpine:3.20",  shell: sh }

  darwin/arm64:
    runner: macos-latest

  darwin/amd64:
    runner: macos-latest
    prefix: ["arch", "-x86_64"]

  windows/amd64:
    runner: windows-latest
    shell: pwsh
    tests:
      - name: version
        command: cmake.exe --version
      - name: smoke
        command: pwsh -File ./tests/smoke.ps1
```

**Fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `runner` | string | Yes | [GitHub Actions][github-actions-docs] runner label |
| `containers` | array | No | Container matrix entries. Absent = native mode. Must have ≥1 entry when present. |
| `containers[].image` | string | Yes | Valid OCI image reference (e.g. `ubuntu:24.04`) |
| `containers[].shell` | string | No* | Shell to invoke inside the container. *Required when image name does not match a known default (see below). |
| `shell` | string | No | Default shell for native legs. Defaults: `pwsh` on Windows, `bash` elsewhere. |
| `prefix` | array of strings | No | Command prefix applied before every test invocation. Defaults: `["arch", "-x86_64"]` on `darwin/amd64` with a `macos-*` runner; empty otherwise. |
| `tests` | array | No | Per-platform test override. When present, replaces the top-level `tests:` array entirely (no partial merge). |

**Platform key validation:**

- Must match `^[a-z0-9_-]+/[a-z0-9_-]+$`.

**Container shell defaults:**

- `alpine*` → `sh`
- `ubuntu*`, `debian*`, `fedora*`, `rocky*`, `opensuse*` → `bash`
- Any other image: `shell` is required.

## `ocx_mirror` {#ocx-mirror}

Pins the `ocx-mirror` version used in generated workflow jobs (`discover`, `prepare`, `push`, `notify`).

```yaml
ocx_mirror:
  release_tag: v0.7.2
  rev: abc123def0123456789012345678901234567890
```

**Fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `release_tag` | string | Yes (when any Linux platform has containers) | ocx-mirror release tag. Used for musl-static artifact download on Linux container legs. Must match `^v\d+\.\d+\.\d+(-[a-z0-9.]+)?$`. |
| `rev` | string | No | Full 40-character git SHA. When set, takes precedence over `release_tag` for `cargo install` paths. When both present, `release_tag` is still used for musl artifact download. Must match `^[0-9a-f]{40}$`. |

When all Linux platforms are container-less (native-only mirror), `release_tag` is optional and `rev` alone is sufficient.

::: info How ocx-mirror is installed in CI
Generated `discover`, `prepare`, `push`, and `notify` jobs install `ocx-mirror` via `cargo install --git ... --rev ${rev}`, cached by [`Swatinem/rust-cache`][swatinem-rust-cache]. A cold install takes roughly 2–3 minutes; a cache hit takes roughly 5 seconds.
:::

## `notify` {#notify}

Configures [Discord][discord] webhook notifications. The webhook fires after the push job completes.

```yaml
notify:
  discord:
    webhook_secret: DISCORD_WEBHOOK_URL
```

**Fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `discord.webhook_secret` | string | Yes (when `notify:` is present) | Name of a [GitHub Actions secret][github-actions-secrets] whose value is the Discord webhook URL. Must match `^[A-Z][A-Z0-9_]+$`. |

**Validation:**

- `webhook_secret` must be a secret name, not a URL. Values containing `discord.com`, `discordapp.com`, or matching `^https?://` are rejected at parse time with exit code 64 (`UsageError`). This prevents accidental commit of a live webhook URL into the repository.

**Notification conditions:**

| Condition | Action |
|-----------|--------|
| All versions already existed in the registry, no failures | Silent (no POST sent) |
| New versions published, no failures | Post green summary with published versions and cascade tags |
| New versions published, some platforms failed | Post yellow summary listing both successes and failures |
| No new versions published, all platforms failed | Post red summary with failure details and run URL |

## Spec inheritance {#inheritance}

`mirror.yml` files support an `extends:` key for shallow merge from a parent spec. Child keys override parent keys at the top level. This is useful for sharing `source` and `assets` across variants of the same tool.

```yaml
extends: ./base-cmake.yml
target:
  registry: private.registry.example.com
  repository: internal/cmake
```

## Example: complete spec {#example}

```yaml
name: cmake
target:
  registry: ocx.sh
  repository: cmake

source:
  github_release:
    owner: Kitware
    repo: CMake
    tag_pattern: "v(?P<version>\\d+\\.\\d+\\.\\d+)$"

assets:
  linux/amd64:
    - "cmake-{{ version }}-linux-x86_64\\.tar\\.gz$"
  darwin/arm64:
    - "cmake-{{ version }}-macos-universal\\.tar\\.gz$"
  windows/amd64:
    - "cmake-{{ version }}-windows-x86_64\\.zip$"

cascade: true

tests:
  - name: version
    command: cmake --version
  - name: ctest
    command: ctest --version

platforms:
  linux/amd64:
    runner: ubuntu-latest
    containers:
      - { image: "ubuntu:24.04", shell: bash }
      - { image: "alpine:3.20",  shell: sh }

  darwin/arm64:
    runner: macos-latest

  windows/amd64:
    runner: windows-latest
    shell: pwsh
    tests:
      - name: version
        command: cmake.exe --version

ocx_mirror:
  release_tag: v0.7.2

notify:
  discord:
    webhook_secret: DISCORD_WEBHOOK_URL
```

<!-- external -->
[github-releases]: https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[github-actions-secrets]: https://docs.github.com/en/actions/security-for-github-actions/security-guides/using-secrets-in-github-actions
[discord]: https://discord.com/developers/docs/resources/webhook
[swatinem-rust-cache]: https://github.com/Swatinem/rust-cache

<!-- commands -->
[cmd-pipeline]: ./command-line.md#ocx-mirror-pipeline
[cmd-sync]: ./command-line.md#ocx-mirror-sync
