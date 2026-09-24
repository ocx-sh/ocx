# Research: cargo-dist pre-host release gate and Bazel external tag

## Metadata

**Date:** 2026-09-22
**Domain:** ci-cd
**Triggered by:** hex-plan for adr_test_speed_tiers (C-PROV release guard, C-UNCACHED)
**Expires:** 2027-03-22

## Direct Answer

**Yes** — cargo-dist 0.31.0 has a supported pre-`host` gate: `local-artifacts-jobs` and/or `global-artifacts-jobs` in `dist-workspace.toml`. Confirmed by decoding the actual generator template (`cargo-dist/templates/ci/github/release.yml.j2`, fetched from axodotdev/cargo-dist via `gh api`), not just prose docs.

## Evidence

Template (`.j2`) shows `host:`'s `needs:` list includes `custom-<job>` for every `local_artifacts_jobs` **and** `global_artifacts_jobs` entry (lines 584–590), and `host`'s `if:` explicitly requires `needs.custom-<job>.result == 'skipped' || needs.custom-<job>.result == 'success'` (lines 596, 598). A failing custom job in either list makes `host`'s `if` false — `host` is skipped, so no upload/release. This repo already proves the pattern live: `plan-jobs = ["./verify-version"]` renders `custom-verify-version`, and `build-local-artifacts` needs it (verified directly in `.github/workflows/release.yml`).

**`host-jobs` is a false lead** despite doc wording ("during which dist decides whether to proceed with publishing"). Template shows `custom-<host_jobs job>` needs `build-global-artifacts` etc., but **`host` itself never needs `host-jobs`** — they run in parallel, only gating `publish`/`announce` afterward. A scan configured as `host-jobs` cannot block the GitHub Release.

`github-build-setup` also doesn't fit: docs say its steps run "before we call `dist build`" — pre-build env setup, not post-build binary scanning.

**Recommendation:** `local-artifacts-jobs = ["./scan-binaries"]` — a `workflow_call` job (own `.yml` under `.github/workflows/`) receiving `plan` output, running after `build-local-artifacts`, gating `build-global-artifacts` → `host` transitively and directly. `dist generate --check` stays green (config-driven, same mechanism as `plan-jobs`/`post-announce-jobs` already in use). Cost: one more job + one more workflow file, `secrets: inherit`.

**Bazel `external` tag** (confirmed via `bazel.build/reference/be/common-definitions`, Bazel 9.x doc unchanged): `external` = "force test to be unconditionally executed (regardless of `--cache_test_results`)" — always reruns, distinct from `no-cache` ("never cached, locally or remotely") and `no-remote-cache` ("never cached remotely"). `external` is not itself a cache-disable tag, just an unconditional-rerun tag; combine with `no-cache`/`no-remote-cache` if remote-cache pollution from a definitely-rerun target is also a concern. A list comprehension over `glob(["tests/test_*.py"])` generating one `sh_test` per file with per-target `tags=[...]` overrides is idiomatic Bazel macro style (same shape already used by this repo's 181-target acceptance suite).

## Sources

- `gh api repos/axodotdev/cargo-dist/contents/cargo-dist/templates/ci/github/release.yml.j2` (ground truth, this session)
- https://github.com/axodotdev/cargo-dist/blob/main/book/src/reference/config.md (plan-jobs, local-artifacts-jobs, global-artifacts-jobs, host-jobs, publish-jobs, post-announce-jobs, github-build-setup)
- https://github.com/axodotdev/cargo-dist/blob/main/book/src/ci/customizing.md
- https://bazel.build/reference/be/common-definitions (tags attribute)
- This repo: `dist-workspace.toml`, `.github/workflows/release.yml`, `.github/workflows/verify-release-ci.yml`
