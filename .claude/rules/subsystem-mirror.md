---
paths:
  - crates/ocx_mirror/**
---

# Mirror Subsystem

Separate crate (`ocx_mirror`) mirror upstream tool releases to OCI registries. YAML-configured, two-phase pipeline.

## Design Rationale

Separate crate: mirror tool standalone binary, own CLI, not part of `ocx` package manager. Two-phase pipeline (prepare concurrent, push sequential) ensure cascade tag order correct — tags push in semver order so `latest` always point to highest version. See `arch-principles.md` for full pattern catalog.

## Module Map

| Path | Purpose |
|------|---------|
| `command/sync.rs` | Main sync command: spec → versions → filter → pipeline |
| `command/check.rs` | Dry-run sync |
| `command/validate.rs` | Spec validation only |
| `command/options.rs` | Shared `SyncOptions` (--exact-version, --latest, --fail-fast) |
| `command/pipeline/mod.rs` | `Pipeline` subcommand dispatcher; routes to generate/plan/prepare/push/notify |
| `command/pipeline/generate/mod.rs` | `generate` subgroup dispatcher |
| `command/pipeline/generate/ci.rs` | `pipeline generate ci` — renderer + `--check` |
| `command/pipeline/plan.rs` | `pipeline plan` — discover new work, emit plan.json |
| `command/pipeline/prepare.rs` | `pipeline prepare --version V` — download + bundle |
| `command/pipeline/push.rs` | `pipeline push` — serial push driver, writes run-summary.json |
| `command/pipeline/notify.rs` | `pipeline notify` — Discord webhook POST |
| `spec/spec.rs` | `MirrorSpec` root, `load_spec()`, extends chain resolution |
| `spec/source.rs` | `Source` enum (GithubRelease, UrlIndex) |
| `spec/target.rs` | `Target` (registry + repository) |
| `spec/assets.rs` | `AssetPatterns` (platform → regex[] mapping) |
| `spec/asset_type.rs` | `AssetTypeConfig` (Archive vs Binary) |
| `spec/versions_config.rs` | Version filter (min/max bounds, new_per_run, backfill order) |
| `spec/verify_config.rs` | Checksum verify options |
| `spec/metadata_config.rs` | Metadata.json path config |
| `spec/concurrency_config.rs` | Parallel download/push limits |
| `spec/tests_config.rs` | `TestEntry` (name + command); top-level `tests:` schema |
| `spec/platforms_config.rs` | `PlatformConfig`, `ContainerConfig`; `platforms:` matrix schema; per-platform version applicability (`min_version`/`max_version`/`exclude` of `ExcludeEntry`+`Severity`) |
| `spec/ocx_mirror_config.rs` | `OcxMirrorConfig` (release_tag + rev); source of `OCX_MIRROR_RELEASE_TAG` |
| `spec/notify_config.rs` | `NotifyConfig`, `DiscordConfig` (`webhook_secret` + `user_id` snowflake); URL-reject validator via `policy_check_notify` |
| `source/github_release.rs` | GitHub API client, tag pattern extraction |
| `source/url_index.rs` | JSON index fetch (remote, inline, generator) |
| `pipeline/orchestrator.rs` | `execute_mirror()`: prepare (concurrent) + push (sequential) |
| `pipeline/download.rs` | HTTP download with resumption |
| `pipeline/verify.rs` | Checksum verify |
| `pipeline/package.rs` | Extract archive, apply metadata, rebundle |
| `pipeline/push.rs` | Push to registry + cascade tag compute |
| `pipeline/mirror_task.rs` | `MirrorTask`: self-contained work unit |
| `pipeline/mirror_result.rs` | `MirrorResult`: Pushed/Skipped/Failed |
| `pipeline.rs` | Shared pipeline helpers (e.g. `propagate_exit_code`) |
| `annotations.rs` | GHA annotation emission for test failures |
| `discord.rs` | Discord webhook HTTP client |
| `junit.rs` | JUnit XML parser; produces `TestResult` per `(V, P, C, name)` |
| `run_summary.rs` | `RunSummary` schema (serialized to run-summary.json) |
| `version_platform_map.rs` | Tracks `(version, platform)` pairs across push legs |
| `normalizer.rs` | `normalize_version()`: add build timestamp |
| `resolver.rs` | `resolve_assets()`: apply regex patterns to asset names |
| `filter.rs` | `filter_versions()`: apply bounds, prerelease skip, backfill cap |
| `error.rs` | `MirrorError` variants and exit code mappings |

## Pipeline Architecture

**Two-phase**: prepare (concurrent) then push (sequential by version).

### Phase 1: Prepare (concurrent)

1. Fetch upstream versions (GitHub API or URL index)
2. Resolve assets per platform (regex match)
3. Filter versions (min/max, prerelease, backfill cap)
4. Parallel: download → verify → bundle (two independent semaphores: I/O vs CPU)

### Phase 2: Push (sequential by version, oldest first)

1. Push bundle to registry
2. Cascade derived tags if enabled (X.Y.Z → X.Y → X → latest)
3. Track pushed (version, platform) pairs for cascade correctness

## Spec Format (YAML)

Key fields: `name`, `target` (registry + repo), `source` (GithubRelease or UrlIndex), `assets` (platform → regex[]), `asset_type` (Archive/Binary), `cascade`, `versions` (min/max/new_per_run/backfill), `verify`, `concurrency`.

Source types:
- `github_release`: `{owner, repo, tag_pattern}` — regex with `(?P<version>...)` capture
- `url_index`: Remote URL, inline versions, or generator command

Spec inheritance via `extends:` (shallow merge, child override parent).

### Per-platform version applicability

`platforms.<p>` carries `min_version` (inclusive) / `max_version` (exclusive) / `exclude` (list of `ExcludeEntry`: single `version` or a `min_version`/`max_version` range, `severity: broken|skip` default `broken`, optional `reason`). The single source of truth is two predicates on `MirrorSpec`: `platform_applies(version, platform)` and `exclude_hit(version, platform)` (both strip build metadata via `parent()` before comparison, reusing the `filter.rs` min-inclusive/max-exclusive convention).

Enforcement choke points:
- **Resolve** — `plan.rs::build_plan_report` + `prepare.rs::build_tasks_for_version` drop non-applicable `(V,P)` via `platform_applies`, so they never reach `plan.json`, are never scheduled/built/tested, and never red the run.
- **Test matrix** — the generated `workflow.yml` test loop skips a version when `matrix.platform ∉ version.platforms` (the discover output already excludes them). Same mechanism fixes the backfill-partial false-red.
- **Push visibility** — `push.rs::collect_excluded_platforms` records `severity: broken` excludes into `VersionSummary.platforms_excluded` (`ExcludedPlatform { platform, reason }`); `skip` stays silent.

To re-enable a pair, delete the entry (next clean run backfills). Use these fields instead of bumping the global `versions.min` for a late-added / dropped / broken platform.

### Discord notify

`notify.discord.user_id` (snowflake, non-secret) is inlined by the renderer into the notify job env as `OCX_MIRROR_DISCORD_USER_ID`. `notify.rs` emits **one embed per version** (avoids Discord's 1024-char field cap), batched ≤10 embeds/message; a message carrying a partial/failed version is prefixed with a scoped `<@id>` mention. `discord.rs` carries `content` + `allowed_mentions` (`parse: []` + explicit `users` so only that user pings). 🔒 rows render `platforms_excluded`.

## Error Model

`MirrorError` enum with exit codes. See `crates/ocx_mirror/src/error.rs::MirrorError::kind_exit_code`.

| Variant | Exit code | Meaning |
|---------|-----------|---------|
| `SpecInvalid` | 65 (DataError) | Schema validation failed |
| `SpecNotFound` | 79 (NotFound) | `mirror.yml` not found at spec path |
| `ExecutionFailed` | 1 (Failure) | Mirror pipeline execution error |
| `SourceError` | 69 (Unavailable) | Upstream source unreachable |
| `SpecUsageError` | 64 (UsageError) | Invalid `mirror.yml` usage: hardcoded webhook URL, empty `tests:`, missing `release_tag` when containers declared, ambiguous shell |
| `RendererDrift` | 65 (DataError) | `--check` mode: generated files differ from current spec |
| `JunitParseError` | 65 (DataError) | JUnit XML parse failure in `pipeline push` |
| `RunSummaryError` | 65 (DataError) | Cannot read or write `run-summary.json` |
| `TemplateError` | 74 (IoError) | Workflow template render failure |
| `WebhookUnavailable` | 69 (Unavailable) | Discord 5xx / timeout in `pipeline notify` |
| `WebhookPermissionDenied` | 77 (PermissionDenied) | Discord 401/403 — webhook secret likely rotated |

## Test Pipeline {#test-pipeline}

`ocx-mirror pipeline` is a family of five subcommands that together implement per-mirror CI pipelines. The pipeline smoke-tests every `(version, platform)` pair before publishing to the registry, preventing broken packages from reaching users.

### Subcommand contracts

| Subcommand | Role in pipeline | Key invariant |
|-----------|-----------------|---------------|
| `pipeline generate ci` | Renderer — writes `.github/workflows/{mirror,describe,verify-generated}.yml` | Idempotent; `--check` exits 65 on drift. Emits `verify-generated.yml` (drift guard, R4) unless `allow_manual_edits: true`. Rejects hardcoded webhook URLs at parse time (R3) |
| `pipeline plan` | Discover — find new work | Side-effect-free; calls registry + source; emits `plan.json` |
| `pipeline prepare --version V` | Prepare — download + bundle | One version across all platforms; writes `bundle-{V}-{P}.tar.xz` per platform |
| `pipeline push` | Push — publish greens | Serial driver; AND across containers for each `(V, P)`; sole cascade-tag writer in pipeline |
| `pipeline notify` | Notify — Discord report | Reads `run-summary.json`; silent when all skipped-existing |

### R1: Cross-mirror concurrency invariant

Generated workflows include a workflow-level `concurrency:` block:

```yaml
concurrency:
  group: mirror-${{ github.workflow }}-publish
  cancel-in-progress: false
```

`cancel-in-progress: false` ensures a push job is never aborted mid-flight, preventing cascade-tag corruption. Different mirror repos use different workflow names so the group key remains repo-scoped.

### R3: Webhook URL rejection invariant

`policy_check_notify` in `spec/notify_config.rs` validates the `discord.webhook_secret` field at spec parse time. Any value matching `discord.com`, `discordapp.com`, or the pattern `^https?://` is rejected with `SpecUsageError` (exit 64) before any file is written. The webhook URL never appears in generated files or in log output.

### R4: Generated drift guard (`verify-generated.yml`)

`pipeline generate ci` emits a third workflow, `.github/workflows/verify-generated.yml`, alongside `mirror.yml` + `describe.yml`. On `pull_request` + push to `main` it runs `ocx-mirror pipeline generate ci --check` (called directly — setup-ocx activates the project toolchain onto PATH), which re-renders from `mirror.yml` and exits 65 on drift — so a hand-edit to any generated workflow fails CI (forbids manual edits to the generated surface). The guard checks all rendered files, including itself.

Opt-out (discouraged): top-level `allow_manual_edits: true` in `mirror.yml`. When set, the renderer omits `verify-generated.yml` and `execute()` prints a stderr note so the disabled guard is never silent. Use only when a repo deliberately maintains its workflows by hand. Field lives on `MirrorSpec` (`spec.rs`), defaults `false`.

### Cross-references

- Design spec: `.claude/artifacts/system_design_mirror_test_pipeline.md` — component contracts, CLI shape, GHA job contracts, install strategy
- ADR: `.claude/artifacts/adr_ocx_mirror_test_pipeline.md` — rationale, risk register, open-call resolutions
- Plan: `.claude/state/plans/plan_mirror_test_pipeline.md` — implementation phases

## Quality Gate

During review-fix loops, run `task rust:verify` — not full `task verify`.
Full `task verify` = final gate before commit.