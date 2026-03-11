# Plan: Mirror Crate Improvements

Three improvements to `ocx_mirror`: typed platform keys, structured sync reporting with `--fail-fast`, and download caching.

---

## 1. Platform as HashMap Key

**Problem**: `AssetPatterns` uses `HashMap<String, Vec<String>>` with string keys like `"linux/amd64"`. The code validates these parse to `Platform` but discards the result. String keys propagate through `resolver`, `filter`, `mirror_task`, `orchestrator`, and `mirror_result` — all stringly-typed.

**Constraint**: `native::Platform` (from `oci_spec` via submodule) doesn't derive `Hash`. We cannot modify the fork for this.

**Solution**: Implement `Hash` on our `Platform` wrapper using its `Display` representation (which is canonical: `{os}/{arch}[/{variant}][/{os_version}]` or `any`).

### Changes

**`crates/ocx_lib/src/oci/platform.rs`**:
- Add `impl Hash for Platform` using `self.to_string().hash(state)` — the `Display` impl is already canonical and deterministic.

**`crates/ocx_mirror/src/spec/assets.rs`**:
- Change `HashMap<String, Vec<String>>` → `HashMap<Platform, Vec<String>>`
- Custom `Deserialize` impl: deserialize as `HashMap<String, Vec<String>>`, then parse keys to `Platform`
- `validate()` no longer needs manual platform parsing (it's done at deser time)
- `compiled()` returns `HashMap<Platform, Vec<Regex>>`

**`crates/ocx_mirror/src/resolver/asset_resolution.rs`**:
- `ResolvedPlatformAsset.platform`: `String` → `Platform`
- `AmbiguousAsset.platform`: `String` → `Platform`

**`crates/ocx_mirror/src/resolver.rs`**:
- `resolve_assets` signature: `patterns: &HashMap<Platform, Vec<Regex>>` → iterates `Platform` keys
- All tests updated to use `Platform` keys

**`crates/ocx_mirror/src/filter.rs`**:
- `ResolvedVersion.platforms`: `Vec<ResolvedPlatformAsset>` — no change needed (inherits from resolver)

**`crates/ocx_mirror/src/pipeline/mirror_task.rs`**:
- `MirrorTask.platform`: `String` → `Platform`

**`crates/ocx_mirror/src/pipeline/mirror_result.rs`**:
- `MirrorResult` variants: `platform: String` → `platform: Platform`

**`crates/ocx_mirror/src/pipeline/orchestrator.rs`**:
- `execute_task`: no more `task.platform.parse::<Platform>()` — it's already a `Platform`
- `platform_slug`: use `platform.ascii_segments().join("_")` instead of `task.platform.replace('/', "_")`

**`crates/ocx_mirror/src/pipeline/package.rs`** (if it takes platform as string):
- Update to accept `&Platform`

**`crates/ocx_mirror/src/command/sync.rs`**:
- `patterns` is now `HashMap<Platform, Vec<Regex>>` — flows naturally
- `MirrorTask` construction uses `platform_asset.platform` (already `Platform`)

---

## 2. Structured Sync Report + `--fail-fast`

**Problem**: Sync results are logged via `tracing` but not reported as structured plain/JSON output. The user sees log lines, not a table. No way to get machine-readable results. No `--fail-fast` option.

**Solution**: Add `--fail-fast` flag + `--format plain|json` flag. Produce a structured report table at the end. On `--fail-fast`, stop orchestrator on first failure.

### Changes

**`crates/ocx_mirror/src/main.rs`** (Cli struct):
- Add `--format plain|json` global flag (default: `plain`)
- Pass format to `Command::execute()`

**`crates/ocx_mirror/src/command.rs`** (Command enum):
- `execute()` takes format parameter

**`crates/ocx_mirror/src/command/sync.rs`** (Sync struct):
- Add `--fail-fast` flag (default: false)
- Add `--format` handling (inherited from global)
- After orchestrator returns `Vec<MirrorResult>`:
  - **Plain format**: print a table: `Version | Platform | Status | Detail`
    - Status: `pushed`, `skipped`, `failed`
    - Detail: digest for pushed, error message for failed
  - **JSON format**: serialize `Vec<MirrorResult>` as JSON array
  - Print summary line: `N pushed, N skipped, N failed`
- Return `Ok(())` if no failures, `Err(ExecutionFailed)` if any failed
- Remove the current log-based reporting (the structured report replaces it)

**`crates/ocx_mirror/src/pipeline/orchestrator.rs`**:
- `execute_mirror` takes `fail_fast: bool` parameter
- On `fail_fast && result.is_err()`: push the Failed result and `break` out of the version loop immediately

**`crates/ocx_mirror/src/pipeline/mirror_result.rs`**:
- Add `Serialize` derive
- Derive `serde::Serialize` with `#[serde(tag = "status", rename_all = "lowercase")]`
- Platform fields use `Platform` (from change 1) which already has `Serialize`

**`crates/ocx_mirror/src/command/sync_all.rs`**:
- Pass through `fail_fast` and `format` to inner `Sync`

**`crates/ocx_mirror/src/command/check.rs`**:
- Pass through format

**Report format examples**:

Plain:
```
Version   | Platform     | Status  | Detail
3.28.0    | linux/amd64  | pushed  | sha256:abc123...
3.28.0    | darwin/arm64 | pushed  | sha256:def456...
3.29.0    | linux/amd64  | failed  | download error: 404
---
3 total, 2 pushed, 0 skipped, 1 failed
```

JSON:
```json
[
  {"version": "3.28.0", "platform": "linux/amd64", "status": "pushed", "digest": "sha256:abc123..."},
  {"version": "3.28.0", "platform": "darwin/arm64", "status": "pushed", "digest": "sha256:def456..."},
  {"version": "3.29.0", "platform": "linux/amd64", "status": "failed", "error": "download error: 404"}
]
```

---

## 3. Download Caching

**Problem**: Every sync re-downloads all files. Partial failures waste bandwidth on retry. No way to resume.

**Solution**: Cache downloads in a local directory (default: `./cache`, configurable via `--cache-dir`). Files are keyed by URL hash. Only delete after successful push. On retry, skip download if cached file exists.

### Cache key strategy

Use the download URL as cache key: `sha256(url)` truncated or the full URL path slugified. Simpler: use `{version}/{platform_slug}/{asset_name}` as the cache path — this is human-readable and matches the temp dir structure.

Cache path: `{cache_dir}/{spec_name}/{version}/{platform_slug}/{asset_name}`

### Changes

**`crates/ocx_mirror/src/command/sync.rs`**:
- Add `--cache-dir` flag (type: `Option<PathBuf>`, default: `None` — meaning `./cache`)
- When `cache_dir` is set/defaulted, pass it to `orchestrator::execute_mirror`

**`crates/ocx_mirror/src/pipeline/orchestrator.rs`**:
- `execute_mirror` takes `cache_dir: &Path` parameter
- Before download: check if `{cache_dir}/{spec_name}/{version}/{platform_slug}/{asset_name}` exists
  - If exists, copy/hardlink to task_dir instead of downloading
  - Log: `"Using cached {asset_name}"`
- After successful push: delete the cached file
  - Log: `"Removed cached {asset_name}"`
- On download (no cache hit): save to cache_dir AND task_dir (or save to cache_dir, symlink to task_dir)

**`crates/ocx_mirror/src/pipeline/download.rs`**:
- No changes needed — the caching logic lives in the orchestrator which decides whether to call `download()`

**`crates/ocx_mirror/src/command/sync_all.rs`**:
- Pass through `--cache-dir`

**`crates/ocx_mirror/src/command/check.rs`**:
- No changes (dry-run doesn't download)

### Cache lifecycle

1. **First run**: download → save to cache_dir → process → push succeeds → delete from cache
2. **Push fails**: download → save to cache_dir → process → push fails → cache file REMAINS
3. **Retry after failure**: cache hit → skip download → process → push succeeds → delete from cache
4. **Cache is user-visible**: `./cache/cmake/3.28.0/linux_amd64/cmake-3.28.0-linux-x86_64.tar.gz`

---

## Implementation Order

1. **Platform Hash** — foundation for all other changes (types flow through everything)
2. **Structured Report** — independent of caching, improves UX immediately
3. **Download Caching** — builds on top of the orchestrator changes

## Files Changed (summary)

| File | Change 1 | Change 2 | Change 3 |
|------|----------|----------|----------|
| `ocx_lib/src/oci/platform.rs` | Hash impl | — | — |
| `ocx_mirror/src/main.rs` | — | --format flag | — |
| `ocx_mirror/src/command.rs` | — | format param | — |
| `ocx_mirror/src/command/sync.rs` | Platform types | --fail-fast, report | --cache-dir |
| `ocx_mirror/src/command/sync_all.rs` | — | pass-through | pass-through |
| `ocx_mirror/src/command/check.rs` | — | pass-through | — |
| `ocx_mirror/src/spec/assets.rs` | HashMap<Platform, _> | — | — |
| `ocx_mirror/src/resolver.rs` | Platform keys | — | — |
| `ocx_mirror/src/resolver/asset_resolution.rs` | Platform fields | — | — |
| `ocx_mirror/src/filter.rs` | (inherits) | — | — |
| `ocx_mirror/src/pipeline/mirror_task.rs` | Platform field | — | — |
| `ocx_mirror/src/pipeline/mirror_result.rs` | Platform field | Serialize | — |
| `ocx_mirror/src/pipeline/orchestrator.rs` | Platform types | fail_fast param | cache logic |
| `ocx_mirror/src/pipeline/download.rs` | — | — | — |
| `ocx_mirror/src/pipeline/push.rs` | — | — | — |
