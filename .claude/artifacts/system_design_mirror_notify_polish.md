# System Design — Mirror Notify Polish

**Status:** draft / handover artifact
**Created:** 2026-05-14
**Branch:** `goat` (folds into `feat/mirror-test-pipeline`)
**Plan parent:** plan_mirror_test_pipeline (Phase S11+, future increment)

## Why

The S1–S10 pipeline lands a green Discord notification skeleton. Three gaps remain:

1. **No package description** — `ocx.sh/<repo>` ships with no readme, no logo, no catalog metadata.
2. **Discord embed too sparse** — a red embed says "failed all platforms" with one line per platform and no link to the responsible job. The Failed platforms field (added 2026-05-14, commit `a9e9063e`) is a v1; we want a tabular layout with per-job links.
3. **Standalone mirror repos** confuse `README.md` (repo doc) with the package catalog file (`ocx package describe --readme`). Currently the `mirrors/<name>/README.md` in the main OCX repo doubles as both — when a mirror lives in its own repo, the root `README.md` must serve repo readers, not the catalog.

This artifact captures the design so we can ship it as a separate PR after the current S10 fixes land.

## Scope

In:
- Convention split: standalone-mirror-repo gains `CATALOG.md` for catalog content; `README.md` is repo-level.
- `mirror.yml` `catalog:` block pointing at the readme + logo files.
- New workflow file `describe.yml` rendered by `pipeline generate ci`, path-filtered on `CATALOG.md` / `logo.*` / `mirror.yml`.
- New `ocx-mirror pipeline describe` subcommand that wraps `ocx package describe` using the spec's catalog block.
- Discord embed redesign: thumbnail (logo), tabular 3-inline-column layout, per-job markdown links.
- `PlatformFailure` schema extension: optional `job_url: Option<String>`.
- Per-leg `junit-{V}-{P}-{C}.url` sidecar emitted by each test job (one GH API call to resolve job ID).

Out:
- Login UX / credential storage (`ocx login`) — separate handover, see *Auth strategy* at the bottom.
- Org-level catalog index (e.g. ocx-sh/.github catalog page) — separate website task.

## File layout — standalone mirror repo

```
mirror-<name>/
├── .github/workflows/
│   ├── mirror.yml       # rendered, sync pipeline
│   └── describe.yml     # rendered, catalog/logo publish
├── ocx/                 # submodule pinning ocx-sh/ocx
├── mirror.yml           # sync config
├── metadata.json        # package metadata (env, entrypoints)
├── CATALOG.md           # package catalog content — frontmatter + body
├── logo.svg             # optional logo
└── README.md            # repo-level: status badges, what this mirror does, how CI works
```

`mirrors/<name>/` directories inside the main OCX repo keep their current `README.md` for now — those are in-tree mirrors, not standalone repos. Migration path: rename `mirrors/<name>/README.md` to `CATALOG.md` (and adjust catalog tooling) as part of a follow-up.

## `mirror.yml` extension

```yaml
catalog:
  readme: CATALOG.md     # default; relative to mirror.yml
  logo: logo.svg         # default; optional — skip arg if file absent
```

Parser additions in `crates/ocx_mirror/src/spec/`:
- New `catalog_config.rs` exposing `CatalogConfig { readme: PathBuf, logo: Option<PathBuf> }` with `Default` implementing the convention above.
- Add `pub catalog: Option<CatalogConfig>` to `MirrorSpec`.

If both files exist and the block is omitted, treat as the default block (zero-config common case). If the explicit block lists a path that doesn't exist, fail spec validation (`SpecUsageError`, exit 64).

## `pipeline describe` subcommand

```
ocx-mirror pipeline describe [--spec ./mirror.yml]
```

Behaviour:
1. Load spec; resolve catalog block (with defaults).
2. Compute identifier from `target.registry + '/' + target.repository`.
3. Invoke `ocx package describe -i <id> --readme <path> [--logo <path>]` as subprocess.
4. Propagate child exit code via `pipeline::propagate_exit_code`.

Implementation lives at `crates/ocx_mirror/src/command/pipeline/describe.rs`. Reuses the same `invoke_ocx` pattern as `push.rs`. No new error variants — `ExecutionFailed` covers subprocess errors.

## `describe.yml` workflow template

```yaml
name: {MIRROR_NAME}-describe

on:
  push:
    branches: [main]
    paths:
      - CATALOG.md
      - logo.*
      - mirror.yml
  workflow_dispatch:

concurrency:
  group: mirror-${{ github.workflow }}-describe
  cancel-in-progress: false

jobs:
  describe:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@{V4_SHA}
        with: { submodules: recursive }
      - uses: Swatinem/rust-cache@{V2_SHA}
        with: { workspaces: ocx }
      - name: Install ocx-mirror + ocx
        run: |
          cargo install --path ocx/crates/ocx_mirror --locked
          cargo install --path ocx/crates/ocx_cli --locked --root "${RUNNER_TEMP}/ocx-host"
          echo "${RUNNER_TEMP}/ocx-host/bin" >> "${GITHUB_PATH}"
      - name: Push description
        env:
          OCX_AUTH_OCX_SH_USER: ${{ secrets.OCX_MIRROR_REGISTRY_USER }}
          OCX_AUTH_OCX_SH_TOKEN: ${{ secrets.OCX_MIRROR_REGISTRY_TOKEN }}
        run: ocx-mirror pipeline describe
```

Renderer changes (`pipeline generate ci`):
- New template file `templates/describe.yml` alongside `workflow.yml`.
- `ci.rs` emits both files; `--check` covers drift on both.
- Path-filter trigger means no churn on per-version mirror runs.

## Discord embed redesign

### Thumbnail (logo)

Extend `DiscordEmbed` in `crates/ocx_mirror/src/discord.rs`:

```rust
#[derive(Debug, Clone, Serialize)]
pub struct DiscordEmbedThumbnail { pub url: String }

#[derive(Debug, Clone, Serialize)]
pub struct DiscordEmbed {
    // existing fields …
    pub thumbnail: Option<DiscordEmbedThumbnail>,
}
```

Logo URL source: GitHub raw URL derived from `GITHUB_REPOSITORY` env at notify time, e.g.
```
https://raw.githubusercontent.com/{owner}/{repo}/main/logo.svg
```
Only set when `logo.*` exists at repo root (probe via `Path::exists()` on the checked-out tree, or always-set + accept Discord falling back to no thumbnail on 404). Prefer the probe — keeps red embeds clean.

### Tabular layout (3 inline fields)

Discord renders runs of `inline: true` fields side-by-side, 3 per row. Replace the current single-blob "Failed platforms" field with three columns:

```
Failed platforms (3.13.1)
| Platform     | Status | Detail                        |
| linux/amd64  | ❌     | [push_error](<job_url>)       |
| linux/arm64  | ❌     | [push_error](<job_url>)       |
| darwin/amd64 | ❌     | [push_error](<job_url>)       |
```

Implementation:
- Three `DiscordEmbedField { inline: true }` — name = column header, value = newline-joined rows.
- Same pattern for green/partial summaries: Platform | Status | Cascade tags.
- Cap rows per column to keep value ≤1024 chars (Discord limit).

### Per-job links

Each platform-failure row gets a markdown link to the GHA job that produced it.

Workflow side — new step inside the test matrix, after the test run:

```yaml
- name: Record job URL
  if: always()
  shell: bash
  env: { GH_TOKEN: ${{ secrets.GITHUB_TOKEN }} }
  run: |
    mkdir -p junit
    URL=$(gh api repos/${{ github.repository }}/actions/runs/${{ github.run_id }}/jobs \
      --jq ".jobs[] | select(.name | contains(\"${{ matrix.platform }}\") and contains(\"${{ matrix.container_id }}\")) | .html_url" \
      | head -n1)
    echo "${URL}" > "junit/junit-${V}-${{ matrix.platform_slug }}-${{ matrix.container_id }}.url"
```

Push side — `push.rs::evaluate_junit` reads the sidecar next to the JUnit XML, sets `PlatformFailure.job_url`. `RunSummary` schema bump = additive field, no migration needed.

Notify side — `build_embed` renders `Detail` column as `[reason](job_url)` when `job_url.is_some()`, else plain `reason`.

### Schema extension

`crates/ocx_mirror/src/run_summary.rs`:

```rust
pub struct PlatformFailure {
    pub platform: String,
    pub reason: String,
    pub failed_tests: Vec<TestFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_url: Option<String>,        // NEW
}
```

Additive optional field. Backward-compatible with the v1 run-summary.

## Open questions

1. **Logo URL fetch vs embed-via-registry.** Embed thumbnail by GitHub raw URL is cheap but couples Discord to the source repo. Pulling from `ocx.sh` via `ocx package info` would source from the canonical artifact but requires the embed builder to network out. Recommendation: GitHub raw URL for v1; revisit if we ever serve logos from the registry CDN.

2. **`describe.yml` consolidation.** Could fold into `mirror.yml` as a `describe` job triggered by path filter, but GHA workflow `paths:` filters apply at the workflow level, not per-job — so a path-filtered `describe` job would still need a separate workflow file or `dorny/paths-filter` action. Two workflows is simpler.

3. **`mirrors/<name>/README.md` migration.** Out of scope for this artifact, but tracking: in-tree mirrors should rename to `CATALOG.md` to match standalone-repo convention. Cross-repo find/replace + catalog renderer update.

## Auth strategy (separate handover)

Out of scope for this artifact, but flagged here because the S10 push failed with HTTP 403 — registry-side ACL issue, not a code bug. Options for a future increment:

- **Status quo**: env vars `OCX_AUTH_<REGISTRY>_USER` / `_TOKEN`. Works in CI; awkward for humans.
- **`ocx login REGISTRY`** subcommand: write creds to `~/.ocx/auth.json` (mirroring `~/.docker/config.json`). Could share the `auth/` module's existing docker-creds fallback.
- **Defer to `docker login`**: rely on the existing `auth/` module picking up Docker creds. Zero new code; needs `docker` available on the runner (already true for matrix containers).

For the current 403: the immediate fix is registry-side — grant the `OCX_MIRROR_REGISTRY_USER` write scope on `sh-ocx-oci-prod/shfmt`. No code change can resolve a server-side authorisation rejection.

## Sequencing

Three independent units, ship in this order, each as a separate commit on `goat`:

1. **`pipeline describe` + `describe.yml`** — touches spec + renderer + new template + new subcommand. Self-contained.
2. **Per-job-URL sidecars** — workflow template step + `evaluate_junit` reader + `PlatformFailure.job_url` additive field.
3. **Embed redesign** — thumbnail + tabular fields + markdown links. Pure notify-side change.

Each unit ≤2 files of production code + matching unit tests. Land them as conventional commits with `feat(mirror):` prefix.
