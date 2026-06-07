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
| `min_version` | string | No | Inclusive lower bound: the first upstream version this platform applies to. See [Version applicability](#platform-version-applicability). |
| `max_version` | string | No | Exclusive upper bound: the first upstream version this platform no longer applies to. |
| `exclude` | array | No | Individual `(version[, range])` holes within the window. See [Version applicability](#platform-version-applicability). |

**Platform key validation:**

- Must match `^[a-z0-9_-]+/[a-z0-9_-]+$`.

### Version applicability {#platform-version-applicability}

Not every platform applies to every release. A platform may be **introduced late** upstream (its first binary ships at some `0.11.7`), **dropped** at a later release (the upstream stops shipping that OS/arch), or carry a **known-broken build** for one specific version. Without a per-platform lever, the only knob is the global `versions.min`/`max`, which moves the window for *all* platforms at once — so a single broken `(version, platform)` either reds the run forever or forces a global version bump that strands the other platforms.

`min_version`, `max_version`, and `exclude` constrain *which versions a platform applies to*. A `(version, platform)` pair outside a platform's window — or matched by an `exclude` entry — is never resolved, scheduled, built, tested, or pushed, and never reds the run. This supersedes the old workaround of bumping the global `versions.min` to dodge a late-added or dropped platform.

```yaml
platforms:
  windows/arm64:
    runner: windows-11-arm
    shell: pwsh
    min_version: "0.11.7"          # platform's first upstream release (inclusive)
    exclude:
      - version: "0.16.0"          # one known-broken release
        reason: "aarch64-windows build-exe segfault"
        severity: broken           # 🔒 row in the Discord report (default)

  darwin/amd64:
    runner: macos-14
    max_version: "11.1.0"          # dropped upstream at 11.1.0 (exclusive)
    exclude:
      - max_version: "9.4.0"       # never built anything below 9.4.0
        severity: skip             # silent — no 🔒 row
```

**`exclude` entry fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `version` | string | One of `version` / range | Exclude exactly this version. Mutually exclusive with `min_version`/`max_version`. |
| `min_version` | string | One of `version` / range | Inclusive lower bound of an excluded range. |
| `max_version` | string | One of `version` / range | Exclusive upper bound of an excluded range. A range may set either bound alone (open-ended). |
| `reason` | string | No | Surfaced in the 🔒 row for `broken` excludes. |
| `severity` | `broken` \| `skip` | No | `broken` (default) drops the pair and surfaces a 🔒 row (plus `reason`); `skip` drops it silently. |

**Semantics:**

- `min_version` is inclusive, `max_version` is exclusive — the same convention as the top-level `versions` bounds.
- An `exclude` entry must set either a single `version` **or** a `min_version`/`max_version` range, not both.
- To re-enable a previously-excluded pair, delete the entry — the next clean run backfills it.
- Validation rejects unparseable bounds and conflicting `exclude` shapes with exit code 65 (`DataError`).

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
    user_id: "123456789012345678"
```

**Fields:**

| Field | Type | Required | Description |
|-------|------|----------|-------------|
| `discord.webhook_secret` | string | Yes (when `notify:` is present) | Name of a [GitHub Actions secret][github-actions-secrets] whose value is the Discord webhook URL. Must match `^[A-Z][A-Z0-9_]+$`. |
| `discord.user_id` | string | No | Discord user ID ([snowflake][discord-snowflake]) to mention on failures. Non-secret — inlined into the workflow as `OCX_MIRROR_DISCORD_USER_ID`. Must match `^[0-9]{17,20}$`. |

**Validation:**

- `webhook_secret` must be a secret name, not a URL. Values containing `discord.com`, `discordapp.com`, or matching `^https?://` are rejected at parse time with exit code 64 (`UsageError`). This prevents accidental commit of a live webhook URL into the repository.
- `user_id` must be the numeric snowflake. A URL or `@mention` paste is rejected with exit code 64 (`UsageError`); any other malformed value is rejected with exit code 65 (`DataError`).

**Messages:**

The report posts **one embed per version** (so a release-heavy run never trips Discord's 1024-character field cap), batched into messages of at most 10 embeds. Each embed lists that version's platforms with a status chip:

| Chip | Meaning |
|------|---------|
| 🟢 | Pushed |
| 🔴 | Test or push failure |
| 🚫 | Expected artifact never arrived (missing bundle / JUnit) |
| 🔒 | Deliberately excluded for this version (a `broken` [`exclude`](#platform-version-applicability) entry), with its reason |

When `user_id` is set, any message that carries a partial or failed version is prefixed with an in-message `<@id>` mention — scoped to that one user, so `@everyone` and role pings never fire. Messages with only successful versions never ping.

**Notification conditions:**

| Condition | Action |
|-----------|--------|
| All versions already existed in the registry, no failures | Silent (no POST sent) |
| New versions published, no failures | Green per-version embeds with published platforms; no mention |
| New versions published, some platforms failed | Yellow/red embeds for the affected versions; mention if `user_id` set |
| No new versions published, all platforms failed | Red embeds with failure details and run URL; mention if `user_id` set |

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
    min_version: "3.20.0"          # cmake windows/amd64 mirrored from 3.20 on
    exclude:
      - version: "3.27.0"
        reason: "windows zip repacked upstream"
        severity: broken
    tests:
      - name: version
        command: cmake.exe --version

ocx_mirror:
  release_tag: v0.7.2

notify:
  discord:
    webhook_secret: DISCORD_WEBHOOK_URL
    user_id: "123456789012345678"
```

<!-- external -->
[github-releases]: https://docs.github.com/en/repositories/releasing-projects-on-github/about-releases
[github-actions-docs]: https://docs.github.com/en/actions/writing-workflows/choosing-what-your-workflow-does/using-pre-written-building-blocks-in-your-workflow
[github-actions-secrets]: https://docs.github.com/en/actions/security-for-github-actions/security-guides/using-secrets-in-github-actions
[discord]: https://discord.com/developers/docs/resources/webhook
[discord-snowflake]: https://discord.com/developers/docs/reference#snowflakes
[swatinem-rust-cache]: https://github.com/Swatinem/rust-cache

<!-- commands -->
[cmd-pipeline]: ./command-line.md#ocx-mirror-pipeline
[cmd-sync]: ./command-line.md#ocx-mirror-sync
