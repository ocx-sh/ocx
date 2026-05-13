# System Design: ocx-mirror Pre-Publish Multi-Runner Test Pipeline

**Companion to:** [`adr_ocx_mirror_test_pipeline.md`](./adr_ocx_mirror_test_pipeline.md)
**Owner:** Architect → Builder
**Status:** Accepted (all open calls resolved 2026-05-13)
**Date:** 2026-05-13

This spec defines component contracts. It does NOT specify implementation files (that is `/swarm-plan`'s job). Contracts cover: CLI shape, on-disk schemas, GHA job interfaces, and inter-job message formats (JUNIT XML + JSON).

---

## Component Map

```
                            ┌─────────────────────────┐
                            │ mirror.yml (hand-edited)│
                            └──────────┬──────────────┘
                                       │
                       ┌───────────────▼────────────────┐
                       │ ocx-mirror generate ci         │  Renderer
                       │ (templates baked via include_str)│
                       └───────────────┬────────────────┘
                                       │
                       ┌───────────────▼──────────────────┐
                       │ .github/workflows/mirror.yml     │  Generated
                       │ scripts/install-ocx.sh           │  artifacts
                       │ scripts/install-branch-protection│
                       └───────────────┬──────────────────┘
                                       │ runs on GHA
            ┌──────────────────────────▼──────────────────────────────────┐
            │ discover ─► prepare ─► test ─► push ─► notify                │
            └─────────────────────────────────────────────────────────────┘
                  │           │          │         │           │
                  │           │          │         │           │
         ocx-mirror plan   ocx-mirror  ocx pkg   ocx-mirror   ocx-mirror
                           prepare    test      push         notify
                           (per V)    -- CMD    (single,     (Discord
                                      → JUNIT   serial,      webhook)
                                      XML       calls
                                                ocx package
                                                push --cascade
                                                --format json)
                                                emits run-summary.json
```

Five new ocx-mirror subcommands consolidated under one `pipeline` subgroup (S3–S9): `ocx-mirror pipeline {generate ci, plan, prepare, push, notify}`. **No new `ocx package` subcommands.** Existing `ocx package test` and `ocx package push --cascade --format json` are reused unchanged. **`ocx` binary obtained via direct `gh release download` from `ocx-sh/ocx` releases** — host legs download glibc/darwin/msvc artifact, container legs download musl-static artifact and mount as `/usr/bin/ocx`. Single env var `OCX_BINARY_OVERRIDE` covers integration-test override. No install script rendered, no third-party setup action. See §5.

---

## 1. `mirror.yml` Schema Additions

Net-new top-level keys: `tests`, `platforms`, `ocx_mirror`, `notify`. Existing `assets`/`source`/`target`/`cascade`/`versions`/`verify`/`concurrency` unchanged (per `adr_ocx_mirror.md`).

### 1.1 `tests:` (new top-level)

```yaml
tests:
  - name: version
    command: cmake --version
  - name: smoke
    command: bash ./tests/smoke.sh
```

Rules:

- Required, must contain ≥1 entry
- Each entry:
  - `name` (string, unique within `mirror.yml`, used as JUNIT testcase name; must match `^[a-zA-Z][a-zA-Z0-9_-]*$`)
  - `command` (single-line string; multi-line scripts must be authored as files in the mirror repo and invoked via shell command, e.g. `bash ./tests/smoke.sh`)
- No `script` field, no auto-detect. Command-only by design (per D6 replacement).

**Env exposed to every test command** (set by renderer in the workflow step):

| Var | Meaning |
|---|---|
| `OCX_INSTALL_DIR` | Path where `ocx package test` materialized the package |
| `OCX_VERSION` | Mirrored version string (e.g., `3.29.0`) |
| `OCX_PLATFORM` | Platform slug (e.g., `linux/amd64`) |
| `OCX_IMAGE` | Container image (empty on native legs) |
| `OCX_TEST_NAME` | The `tests[].name` value for this invocation |

### 1.2 `platforms:` (new top-level)

```yaml
platforms:
  linux/amd64:
    runner: ubuntu-latest               # GHA runner label
    containers:                         # presence = ocx-in-container mode (D8)
      - { image: "ubuntu:24.04", shell: bash }
      - { image: "alpine:3.20",  shell: sh }
      - { image: "fedora:40",    shell: bash }

  linux/arm64:
    runner: ubuntu-24.04-arm
    containers:
      - { image: "ubuntu:24.04", shell: bash }
      - { image: "alpine:3.20",  shell: sh }

  darwin/arm64:
    runner: macos-latest                # absent containers = native mode
    shell: bash                         # default = bash on macos

  darwin/amd64:
    runner: macos-latest
    prefix: ["arch", "-x86_64"]         # defaults from A8 table

  windows/amd64:
    runner: windows-latest
    shell: pwsh                         # default = pwsh on windows
    tests:                              # optional: replaces top-level tests entirely
      - name: version
        command: cmake.exe --version
      - name: smoke
        command: pwsh -File ./tests/smoke.ps1
```

**Validation rules:**

- Platform key must match `^[a-z0-9_-]+/[a-z0-9_-]+$` (parses via existing `Platform` type per `arch-principles.md`).
- `runner` required.
- `containers[]` may be absent (native mode); if present must have ≥1 entry.
- `containers[].image` must be valid OCI reference.
- `containers[].shell` defaults per A9:
  - image starts with `alpine` → `sh`
  - image starts with `ubuntu`/`debian`/`fedora`/`rocky`/`opensuse` → `bash`
  - otherwise required to be explicit
- `prefix` defaults per A8:
  - `darwin/amd64` on `macos-*` runner → `["arch", "-x86_64"]`
  - else empty
- `tests:` per-platform shadows top-level entirely (no partial override).

### 1.3 `ocx_mirror:` (new top-level)

```yaml
ocx_mirror:
  release_tag: v0.7.2                   # ocx-mirror release tag (required if any linux platform has containers; see A11)
  rev: abc123def...                     # optional 40-hex git SHA; if set, supersedes release_tag for cargo-install path
```

Validation (A11 resolution: musl-tagged-release):

- `release_tag` is **required** when any `platforms.<P>.containers:` is non-empty (ocx-in-container linux legs need the musl artifact via `gh release download`).
- `release_tag` matches `^v\d+\.\d+\.\d+(-[a-z0-9.]+)?$`.
- `rev` (if set) matches `^[0-9a-f]{40}$`. Used for `cargo install --git --rev` paths (renderer + non-linux install scripts). When both present, `rev` wins for cargo install; `release_tag` still used for musl-asset download.
- When all linux platforms are container-less (native-only mirror), `release_tag` becomes optional; `rev` alone is sufficient.

### 1.4 `notify:` (new top-level)

```yaml
notify:
  discord:
    webhook_secret: DISCORD_WEBHOOK_URL  # name of GHA secret, NOT a URL
```

**Validation (R3 mitigation):**

- `webhook_secret` value must match `^[A-Z][A-Z0-9_]+$` (GHA secret naming).
- Renderer rejects any value containing `discord.com`, `discordapp.com`, or matching `^https?://`.

---

## 2. CLI Surface — `ocx-mirror` Subcommands

Five new subcommands. All return `ExitCode` aligned with `quality-rust-exit_codes.md`. All emit JSON to stdout when invoked with `--format json` or detected via `GITHUB_ACTIONS=true`.

### 2.1 `ocx-mirror pipeline generate ci`

```
ocx-mirror pipeline generate ci [--check] [--spec <path>]
```

**Inputs:** `mirror.yml` at `--spec` (default `./mirror.yml`).

**Outputs (write mode):**
- `.github/workflows/mirror.yml` (overwrite)
- `scripts/install-ocx.sh` (overwrite, if any linux platform has containers)
- `scripts/install-branch-protection.sh` (overwrite, idempotent)
- `README.md` snippet appended/updated with required-check list (A6)

**Outputs (--check mode):** exit 0 if all generated files match what would be produced; exit 65 (DataError) on drift. Stderr emits path-only diff hints to avoid leaking secret-substituted content.

**Generated-file headers:**

```yaml
# DO NOT EDIT — generated by ocx-mirror generate ci
# Source: mirror.yml
# Renderer version: ocx-mirror {VERSION} ({GIT_SHA_SHORT})
```

**Exit codes:**

| Code | Meaning |
|---|---|
| 0 | Success |
| 64 (UsageError) | `mirror.yml` not found, hardcoded webhook URL, bad runner label, empty `tests:`, ambiguous shell |
| 65 (DataError) | Drift detected in `--check`, or schema invalid |
| 74 (IoError) | Write failure |

### 2.2 `ocx-mirror pipeline plan`

```
ocx-mirror pipeline plan [--spec <path>] [--format json|plain]
```

**Purpose:** Isolate the "what new versions need work" computation so the GHA `discover` job can call it side-effect-free.

**Inputs:** `mirror.yml`; network access to source (GitHub releases / URL index) + target registry (tag list).

**Output (stdout, JSON):**

```json
{
  "schema_version": 1,
  "has_new": true,
  "versions": [
    { "version": "3.29.0", "platforms": ["linux/amd64", "linux/arm64", "darwin/arm64", "darwin/amd64", "windows/amd64"], "kind": "new" },
    { "version": "3.28.5", "platforms": ["linux/arm64"], "kind": "backfill-partial" }
  ],
  "target": "ocx.sh/cmake",
  "ocx_mirror_rev": "abc123..."
}
```

`kind` values: `new` | `backfill-partial`. Exit 0 even when `has_new: false`; 69 (Unavailable) on source/registry network failure.

### 2.3 `ocx-mirror pipeline prepare`

```
ocx-mirror pipeline prepare --version <V> [--spec <path>] [--work-dir <dir>]
```

**Purpose:** Run Phase-1 prepare (download → verify → bundle) for one version across all declared platforms. Mirrors the per-version subset of the existing `command/sync.rs` Phase-1 loop, factored into its own entry point.

**Inputs:** `mirror.yml`, `--version V`. Network access for download + verify.

**Outputs (filesystem):**
- `{work_dir}/{V}/{platform_slug}/bundle.tar.xz` per declared platform
- `{work_dir}/{V}/manifest.json` listing bundles with sizes + digests

**Exit codes:**

| Code | Meaning |
|---|---|
| 0 | All platforms prepared |
| 69 (Unavailable) | Source unreachable |
| 65 (DataError) | Checksum mismatch on any platform |
| 74 (IoError) | Disk failure |

### 2.4 `ocx-mirror pipeline push`

```
ocx-mirror pipeline push --bundles-dir <dir> --junit-dir <dir> --write-summary <path> [--spec <path>]
```

**Purpose:** Single serial push driver. Aggregates JUNIT results across containers, ANDs per `(V, P)`, calls `ocx package push --cascade -p <P> --format json` for greens in deterministic order (oldest V first, then platform order from spec), accumulates per-push JSON into `run-summary.json`. **Sole writer of cascade tags in the pipeline.**

**Inputs:**
- `mirror.yml` (for declared platform set, `tests[].name` list, target identifier)
- `--bundles-dir`: directory containing `bundle-{V}-{platform_slug}.tar.xz` files (downloaded artifacts)
- `--junit-dir`: directory containing `junit-{V}-{platform_slug}-{container_id}.xml` files

**Per-`(V, P)` go/no-go rule (D7):**
- For each container `C` declared for `P`: parse `junit-{V}-{P}-{C}.xml`, check zero failures + zero errors + every declared `tests[].name` present
- AND across all containers; if any missing or failed → `(V, P)` failed (no push)
- Native platforms: single container_id `_native_` (renderer convention) → same AND-of-one logic

**Push call shape (existing CLI, unchanged):**

```
ocx package push --cascade -p <P> -i <target>:<V> <bundle.tar.xz> --format json
```

The `--format json` output is the existing Printable JSON for `package push` (per `subsystem-cli.md` `Api` reporting layer); push command consumes this. Expected fields (S2 verifies schema):
- `manifest_digest`: pushed manifest digest
- `cascade_tags_written`: array of tag strings written (e.g., `["3.29.0", "3.29", "3", "latest"]`)
- `status`: `pushed` / `skipped_existing`

**Output: `run-summary.json`** (consumed by notify, schema §3.1):

```json
{
  "schema_version": 1,
  "mirror": "cmake",
  "target": "ocx.sh/cmake",
  "run_url": "https://github.com/ocx-sh/mirror-cmake/actions/runs/...",
  "versions": [
    {
      "version": "3.29.0",
      "status": "published",
      "platforms_pushed": ["linux/amd64", "linux/arm64", "darwin/arm64", "darwin/amd64", "windows/amd64"],
      "platforms_failed": [],
      "cascade_tags_written": ["3.29.0", "3.29", "3", "latest"],
      "test_failures": []
    },
    {
      "version": "3.28.5",
      "status": "partial",
      "platforms_pushed": ["linux/arm64"],
      "platforms_failed": [
        {
          "platform": "darwin/amd64",
          "reason": "test_failed",
          "failed_tests": [
            { "test": "smoke", "container": "_native_", "message": "arch -x86_64: binary not found" }
          ]
        }
      ],
      "cascade_tags_written": ["3.28.5"],
      "test_failures": [
        { "version": "3.28.5", "platform": "darwin/amd64", "container": "_native_", "test": "smoke", "message": "..." }
      ]
    }
  ],
  "any_red": true,
  "any_new_green": true
}
```

`status` per D12 status table:

| Status | Trigger |
|---|---|
| `published` | All declared platforms pushed; `latest` written iff this is newest version in this run |
| `partial` | Some pushed, some failed; `latest` NOT written even if newest |
| `failed` | None pushed |
| `skipped_existing` | All declared platforms already at registry |
| `skipped_executor` | Phase-2 placeholder (no executor for declared platform) |

**Exit codes:**

| Code | Meaning |
|---|---|
| 0 | Push job succeeded (any version outcome accepted, including all-failed; summary written) |
| 69 (Unavailable) | Registry unreachable mid-push |
| 74 (IoError) | Cannot read JUNIT/bundles or write summary |

### 2.5 `ocx-mirror pipeline notify`

```
ocx-mirror pipeline notify --run-summary <path> --webhook-env-var <NAME>
```

**Purpose:** Read `run-summary.json`, emit Discord webhook POST per D10 taxonomy. Webhook URL sourced from env var `<NAME>` (set in workflow from `${{ secrets.DISCORD_WEBHOOK_URL }}`).

**Notify rules (D10 + D11):**

| Condition | Action |
|---|---|
| `versions[*].status` all `skipped_existing` AND no `test_failures` | silent (exit 0, no POST) |
| `any_new_green && !any_red` | post: `📦 <tool>: published <V> (<platforms>) — cascade: <tags>` |
| `any_new_green && any_red` | post: `⚠️ <tool> <V> partial — failed: <P> (<test>: <msg>). Published: <good-Ps>. Run: <URL>` |
| `!any_new_green && any_red` | post: `❌ <tool> <V> failed all platforms — see <URL>` (with `failed_tests` summary) |

**Payload shape** (Discord webhook JSON):

```json
{
  "username": "ocx-mirror",
  "embeds": [
    {
      "title": "📦 cmake: published 3.29.0",
      "color": 3066993,
      "fields": [
        { "name": "Platforms", "value": "linux/amd64, linux/arm64, darwin/arm64, darwin/amd64, windows/amd64", "inline": false },
        { "name": "Cascade", "value": "3.29.0, 3.29, 3, latest", "inline": false }
      ],
      "url": "https://github.com/ocx-sh/mirror-cmake/actions/runs/..."
    }
  ]
}
```

Color codes: green `0x2ECC71`, yellow `0xF1C40F`, red `0xE74C3C`.

**Exit codes:**

| Code | Meaning |
|---|---|
| 0 | Posted (or intentionally silent) |
| 69 (Unavailable) | Discord 5xx / timeout |
| 77 (PermissionDenied) | Webhook URL rejected (401/403) — secret rotated |

---

## 3. Inter-Job Message Schemas

### 3.1 JUNIT XML (test job → push job)

Each `(V, P, C)` test leg emits `junit-{V}-{platform_slug}-{container_id}.xml`. One `<testsuite>` per file; one `<testcase>` per entry in mirror.yml `tests[]`.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<testsuites>
  <testsuite name="ocx-mirror.cmake.linux_amd64.ubuntu_2404"
             tests="2" failures="0" errors="0" skipped="0"
             timestamp="2026-05-13T10:24:31Z" time="9.4">
    <properties>
      <property name="ocx.version" value="3.29.0"/>
      <property name="ocx.platform" value="linux/amd64"/>
      <property name="ocx.image" value="ubuntu:24.04"/>
    </properties>
    <testcase name="version" classname="ocx-mirror.cmake.linux_amd64.ubuntu_2404" time="4.1"/>
    <testcase name="smoke"   classname="ocx-mirror.cmake.linux_amd64.ubuntu_2404" time="5.3"/>
  </testsuite>
</testsuites>
```

On test failure: `<testcase>` contains `<failure message="..." type="exit_code">stderr tail</failure>`. Convention: `type="exit_code"` for non-zero exit, `type="timeout"` for time exceeded.

Native legs use `container_id = _native_` literal.

### 3.2 Inter-job artifact summary

| Message | Producer | Consumer | Format |
|---|---|---|---|
| `plan.json` (workflow outputs) | discover | (next jobs via needs) | JSON, §2.2 |
| `bundle-{V}-{platform_slug}.tar.xz` | prepare | test, push | binary tar.xz |
| `junit-{V}-{platform_slug}-{container_id}.xml` | test | push, `publish-unit-test-result-action` | JUNIT XML §3.1 |
| `run-summary.json` | push | notify | JSON, §2.4 |

All JSON messages carry `schema_version: 1`; future bumps follow `quality-rust.md` "Version Enum via `serde_repr`" pattern.

---

## 4. GHA Workflow Contract

Generated `.github/workflows/mirror.yml`. Job-level contracts below; raw YAML lives in renderer templates (S3).

### 4.1 Triggers

- `schedule:` cron from `mirror.yml: versions.poll_interval` (if present)
- `workflow_dispatch:` always
- `push:` on default branch, path-filtered to `mirror.yml`, `scripts/**`, `.github/workflows/mirror.yml`

### 4.2 Job: `discover`

- Runs on: `ubuntu-latest`
- Steps:
  1. Checkout
  2. Install ocx-mirror (per `ocx_mirror.rev`, see §5.1)
  3. `ocx-mirror plan --format json > plan.json`
  4. Set outputs from plan.json: `has_new`, `versions`, `target`
- Outputs: `has_new` (string), `versions` (JSON array), `target` (string)

### 4.3 Job: `prepare`

- `if: needs.discover.outputs.has_new == 'true'`
- Runs on: `ubuntu-latest`
- Strategy: `matrix.version: ${{ fromJson(needs.discover.outputs.versions) }}`
- Steps:
  1. Checkout, install ocx-mirror
  2. `ocx-mirror prepare --version ${{ matrix.version.version }}`
  3. Upload artifact `bundle-${{ matrix.version.version }}` containing all `bundle-{V}-{platform_slug}.tar.xz` files (retention 1 day)

### 4.4 Job: `test`

- `if: needs.discover.outputs.has_new == 'true'`
- `needs: [discover, prepare]`
- Strategy:
  - `matrix`: static expansion of `(platform, container)` from `mirror.yml`; `fail-fast: false`
  - Native platforms generate one matrix row with `container.image = _native_`
- Runs on: `${{ matrix.runner }}`
- Steps per leg:
  1. Checkout
  2. Download all `bundle-*` artifacts
  3. Install ocx in execution environment (§5.2)
  4. For each `version` in `needs.discover.outputs.versions`:
     - For each `test` in `mirror.yml` `tests[]` (or per-platform override):
       - Container leg: `docker run --rm -v <bundle>:/bundle.tar.xz:ro -v <ocx>:/usr/local/bin/ocx:ro -e OCX_VERSION=$V -e OCX_PLATFORM=$P -e OCX_IMAGE=$IMAGE -e OCX_TEST_NAME=$NAME <image> <shell> -c 'ocx package test -p $P /bundle.tar.xz -- <command>'`
       - Native leg: `<prefix...> ocx package test -p $P bundle.tar.xz -- <shell> -c '<command>'`
     - Record exit code + duration + stderr tail per test
  5. Synthesize `junit-{V}-{platform_slug}-{container_id}.xml` covering all tests for all versions for this `(P, C)` leg
  6. Upload artifact `junit-${{ matrix.platform_slug }}-${{ matrix.container_id }}` (retention 1 day)

**Matrix entry naming for stable check names (D4):**
- `container_id` slug: `${image}` with `:` and `/` replaced by `_` (e.g., `ubuntu:24.04` → `ubuntu_2404`)
- `platform_slug`: existing `Platform::ascii_segments().join("_")` from `arch-principles.md`
- Check name: `test (linux/amd64, ubuntu_2404)` — stable across runs

### 4.5 Job: `push`

- `if: needs.discover.outputs.has_new == 'true'`
- `needs: [discover, prepare, test]`
- Runs on: `ubuntu-latest` (single job, no matrix)
- Steps:
  1. Download all `bundle-*` and `junit-*` artifacts into `./_artifacts/`
  2. Install ocx-mirror + ocx (native, used to call `ocx package push`)
  3. `ocx-mirror push --bundles-dir ./_artifacts/bundles --junit-dir ./_artifacts/junit --write-summary run-summary.json`
  4. Upload `run-summary.json` artifact (retention 1 day)
  5. Upload all junit XMLs to `EnricoMi/publish-unit-test-result-action@v2` for PR annotations
- Outputs: `any_new_green` (parsed from summary), `any_red` (parsed from summary)

### 4.6 Job: `notify`

- `if: needs.discover.outputs.has_new == 'true' && (needs.push.outputs.any_new_green == 'true' || needs.push.outputs.any_red == 'true')`
- `needs: push`
- Runs on: `ubuntu-latest`
- Steps:
  1. Download `run-summary.json`
  2. `ocx-mirror notify --run-summary run-summary.json --webhook-env-var DISCORD_WEBHOOK_URL`
- Env: `DISCORD_WEBHOOK_URL: ${{ secrets.DISCORD_WEBHOOK_URL }}`

### 4.7 Workflow-level concurrency (R1 mitigation)

```yaml
concurrency:
  group: mirror-${{ github.workflow }}-publish
  cancel-in-progress: false
```

Serializes runs against same registry repo. `cancel-in-progress: false` — never abort a push mid-flight.

---

## 5. Install Strategy

**Principle:** plain `gh release download` of pinned `ocx-sh/ocx` release artifacts everywhere. No install script rendered per-mirror. No third-party setup action. Auto-update via `ocx` itself (the primary value-add of `setup-ocx`) is a non-goal in CI scope. Single workflow-level env var `OCX_BINARY_OVERRIDE` covers the integration-test "use locally-built binary" case.

### 5.1 ocx-mirror in `discover` / `prepare` / `push` / `notify` jobs

```bash
cargo install --git https://github.com/ocx-sh/ocx --rev "${OCX_MIRROR_REV}" --bin ocx_mirror
```

Cached via `Swatinem/rust-cache@v2` keyed on `${OCX_MIRROR_REV}`. Cold ~2–3 min; cached ~5s. (`ocx-mirror` ships from the same workspace as `ocx` but cargo-install bootstrap stays — `ocx-mirror` does not have its own release artifact today.)

Env override hook for development:

```bash
OCX_MIRROR_SOURCE=path:/workspace/ocx                                # local checkout
OCX_MIRROR_SOURCE=git+https://github.com/ocx-sh/ocx?rev=abc123       # explicit
OCX_MIRROR_SOURCE=git+https://github.com/ocx-sh/ocx?branch=main      # head
```

### 5.2 ocx on host legs (push/notify on ubuntu-latest; native test legs on macos/windows)

```bash
TAG="$OCX_MIRROR_RELEASE_TAG"
ARCH=$(uname -m)  # x86_64 / aarch64 / arm64
case "$RUNNER_OS" in
  Linux)   TARGET="${ARCH}-unknown-linux-gnu";  EXT="tar.xz" ;;
  macOS)   TARGET="${ARCH}-apple-darwin";       EXT="tar.xz" ;;
  Windows) TARGET="${ARCH}-pc-windows-msvc";    EXT="zip"    ;;
esac

if [ -n "${OCX_BINARY_OVERRIDE:-}" ]; then
  cp "$OCX_BINARY_OVERRIDE" "$RUNNER_TEMP/ocx-host/ocx"   # integration tests
else
  gh release download "$TAG" --repo ocx-sh/ocx \
    --pattern "ocx-${TARGET}.${EXT}" --output - \
    | tar -xJf - -C "$RUNNER_TEMP/ocx-host"               # unzip for windows
fi
echo "$RUNNER_TEMP/ocx-host" >> "$GITHUB_PATH"
```

`OCX_MIRROR_RELEASE_TAG` sourced from `mirror.yml: ocx_mirror.release_tag`. Cached via `actions/cache@<pinned-sha>` keyed on `${{ runner.os }}-${{ runner.arch }}-ocx-${TAG}`. Override path skips cache (always fresh). Cold ~3s; cached ~50ms.

### 5.3 ocx inside container (linux test leg, A11 resolution: musl-tagged-release)

Container leg uses musl-static artifact. **Single download path on host via `gh release download`** (`gh` CLI inherits `GITHUB_TOKEN` from runner — no extra auth wiring). The container then consumes the local binary via a per-leg ephemeral Dockerfile `ADD` step. Picking `ADD` over a runtime `-v` mount has two benefits: (1) layer cache hits when the same tar.xz is reused, (2) integration-test override is a single-knob change (point the env var at a different local tar; everything else identical).

**Host step — download (or override) once per leg:**

```bash
ARCH=$(uname -m)  # x86_64 or aarch64
TAG="$OCX_MIRROR_RELEASE_TAG"

if [ -n "${OCX_BINARY_OVERRIDE_MUSL:-}" ]; then
  cp "$OCX_BINARY_OVERRIDE_MUSL" "$RUNNER_TEMP/ocx-musl.tar.xz"
else
  gh release download "$TAG" --repo ocx-sh/ocx \
    --pattern "ocx-${ARCH}-unknown-linux-musl.tar.xz" \
    --output "$RUNNER_TEMP/ocx-musl.tar.xz"
fi

tar -xJf "$RUNNER_TEMP/ocx-musl.tar.xz" -C "$RUNNER_TEMP/ocx-musl"
```

`gh release download` uses the runner's auth (`GITHUB_TOKEN`) automatically; private releases work without explicit credential wiring. `OCX_BINARY_OVERRIDE_MUSL` is a single workflow-level env var that, when set, points at a pre-positioned tar.xz on the runner filesystem (typically uploaded via `actions/upload-artifact` + `download-artifact` in an integration-test pipeline). The override switches the source of the tar; everything downstream is identical.

**Container step — per-leg ephemeral Dockerfile:**

```bash
cat >"$RUNNER_TEMP/ocx-musl/Dockerfile" <<'EOF'
ARG BASE_IMAGE
FROM ${BASE_IMAGE}
ADD ocx /usr/bin/ocx
RUN chmod +x /usr/bin/ocx
EOF

IMAGE_TAG="ocx-test-${PLATFORM_SLUG}-${CONTAINER_ID}:${OCX_MIRROR_RELEASE_TAG}"

docker build \
  --build-arg "BASE_IMAGE=${CONTAINER_IMAGE}" \
  --tag "$IMAGE_TAG" \
  "$RUNNER_TEMP/ocx-musl"

docker run --rm \
  -v "$RUNNER_TEMP/bundle.tar.xz:/bundle.tar.xz:ro" \
  -e OCX_VERSION -e OCX_PLATFORM -e OCX_IMAGE -e OCX_TEST_NAME \
  "$IMAGE_TAG" \
  <shell> -c 'ocx package test --platform <P> /bundle.tar.xz -- <test command>'
```

Why per-leg Dockerfile instead of `-v` mount:

- **Layer cache.** `ADD ocx /usr/bin/ocx` is a deterministic layer keyed on the tar contents — `docker build` is a no-op when the same `(base image, ocx tar)` pair was built earlier in the workflow (cross-job sharing via `docker save | gzip > artifact` if budget for it; phase 1 keeps within-job cache only).
- **Match production install topology.** Real users install ocx into containers with `ADD` (or `COPY`) at image-build time, not at runtime. Tests use the same path so a smoke-test pass means a real Dockerfile install would also work.
- **Single-knob integration test override.** `OCX_BINARY_OVERRIDE_MUSL` swaps the source tar; the Dockerfile and `docker build` step never change. No conditional logic in the build path.
- **Read-only enforcement.** `RUN chmod +x` baked into image; no `:ro` flag plumbing on runtime mount.

For non-CI Dockerfile users building ocx-bearing images outside this pipeline, the equivalent instruction is the public asset URL form:

```dockerfile
ARG OCX_VERSION=latest
ARG ARCH=x86_64
ADD https://github.com/ocx-sh/ocx/releases/download/${OCX_VERSION}/ocx-${ARCH}-unknown-linux-musl.tar.xz /tmp/ocx.tar.xz
RUN tar -xJf /tmp/ocx.tar.xz -C /usr/bin/ && rm /tmp/ocx.tar.xz && chmod +x /usr/bin/ocx
```

CI workflow uses the local-filesystem form so the host-side download (with `GITHUB_TOKEN` auth + override knob) is the single source of truth.

---

## 6. Testability Strategy per Component

| Component | Test approach | Test scope |
|---|---|---|
| `mirror.yml` schema | serde round-trip + invalid-spec rejection | Unit (Rust) |
| Renderer (`generate ci`) | Golden tests against fixture mirror.ymls | Unit + integration |
| Renderer (`--check`) | Mutate generated file, assert exit 65 + non-empty stderr | Unit |
| `ocx-mirror plan` | Fake registry + fake source adapters | Unit |
| `ocx-mirror prepare` | Fake source + filesystem-output assertion | Unit + integration |
| `ocx-mirror push` | Synthetic JUNIT fixtures + fake registry; covers D12 status table | Unit |
| `ocx-mirror notify` | Golden tests on Discord payload JSON for each D10 outcome | Unit |
| `ocx package push --cascade --format json` | Schema audit (S2) — verify `manifest_digest` + `cascade_tags_written` fields stable | Unit |
| Generated workflow | `act` local runner (or synthetic GHA env) on small fixture | Integration |
| End-to-end | Real mirror repo + `registry:2` test registry + test Discord channel | Acceptance (pytest) |

---

## 7. What This Spec Does NOT Decide

These belong to `/swarm-plan` or downstream:

- File layout under `crates/ocx_mirror/src/command/` — which subcommands live where, how to refactor existing `command/sync.rs` to share Phase-1 logic with `prepare` subcommand
- Trait boundaries (e.g., `Renderer`, `Notifier`)
- Cache keys for cargo-install across mirror repos
- First-mirror bootstrap path (`ocx-sh/mirror-shfmt` setup script + initial PR template)

---

## 8. Open Items

None. All ADR open calls resolved 2026-05-13:

- A1 → do nothing (no flag change to `ocx package test`)
- A4 → deferred (no fan-out tooling in phase 1)
- A11 → musl-tagged-release (`ocx_mirror.release_tag` required for linux container legs)

Ready for `/swarm-plan`.

---

## Changelog

| Date | Author | Change |
|------|--------|--------|
| 2026-05-13 | Architect (Claude) | Initial draft |
| 2026-05-13 | Architect (Claude) | Revised after user feedback: single serial push, tests:[]+JUNIT, command-only, drop cascade subcommand |
| 2026-05-13 | Architect (Claude) | A11 locked to musl-tagged-release; A1/A4 closed. Status → Accepted. |
| 2026-05-13 | Architect (Claude) | Plan-handoff refinements: (1) CLI consolidated under `ocx-mirror pipeline` subgroup (§2.1–§2.5 retitled); (2) install delegated to upstream `ocx-sh/setup-ocx` (§5 rewritten; new §1.2.5 `ocx_install:` block forwarding to setup-ocx env knobs); (3) renderer drops `install-ocx.sh` artifact, retains workflow YAML + `install-branch-protection.sh` + README snippet only. |
| 2026-05-13 | Architect (Claude) | `ocx_install:` schema trimmed to **released setup-ocx v1.0.0 input surface** (`version`, `libc`, `setup_ocx_ref`). Env-knob surface (`base_url`, `format_url`, `force`, etc.) exists only on unreleased setup-ocx branch — deferred until upstream tags release with those knobs. Container leg uses plain `gh release download` (no env-knob respect) since setup-ocx ships no container path today. §1.2.5 + §5.2 + §5.3 updated. |
| 2026-05-13 | Architect (Claude) | **Final install pivot:** `ocx_install:` block removed entirely (§1.2.5 deleted). All legs use direct `gh release download` of `ocx-sh/ocx` release artifacts (`-unknown-linux-gnu` / `-apple-darwin` / `-pc-windows-msvc` on host; `-unknown-linux-musl` mounted at `/usr/bin/ocx` in container). Two env-var overrides (`OCX_BINARY_OVERRIDE` for host/native, `OCX_BINARY_OVERRIDE_MUSL` for container) handle integration-test use case. setup-ocx repo retains shell/Dockerfile/devcontainer install role; CI matrix scope no longer depends on it. §5 fully rewritten. |
| 2026-05-13 | Architect (Claude) | Container install switched from `-v` runtime mount to **per-leg ephemeral Dockerfile `ADD`**. Host always runs `gh release download` (single auth path via `GITHUB_TOKEN` inherited by `gh` CLI). Container consumes the local tar contents via `FROM ${BASE} / ADD ocx /usr/bin/ocx / RUN chmod +x`. Benefits: `docker build` layer cache when same `(base image, ocx tar)` pair recurs; matches production Dockerfile install topology; single-knob integration-test override (swap source tar, Dockerfile unchanged). §5.3 rewritten to use `docker build` against an ephemeral Dockerfile in `$RUNNER_TEMP/ocx-musl/`. |
