# Research: PR #637 round 2 — cache, tiers, test tooling, release gate

**Date:** 2026-10-22 · **Consumer:** [`plan_pr637_round2.md`](./plan_pr637_round2.md) ·
**Inputs (untracked, `.agents/` is gitignored):** `.agents/batches/pr637-batch2.md` (findings 1-17, binding),
`.agents/batches/pr637-batch2-research/{conventions-tier-research.json,issues-631-638-triage.json,issues-631-638-analysis.txt}`.
This file keeps the facts the plan's decisions rest on. Line numbers are at `4c6cfa68a`; re-anchor by symbol.

## Q1 Bazel remote cache today (#638)

- `.github/actions/bazel-cache-rc` + `write_rc.py`: read mode writes `build --remote_header=authorization=Basic …`;
  write mode writes a 0700 credential helper + `build --credential_helper=bazel-cache.ocx.sh=…`; rc mode 0600;
  both inputs set or an existing credential → refusal; neither → no file, anonymous read. The action never emits
  `--remote_upload_local_results`; `.bazelrc` sets it `false`. Cache user `ci` is in both realms.
- Only one upload grant exists: `verify-basic.yml` smoke, `task bazel:test:unit -- …` with
  `--remote_upload_local_results=true` via `CLI_ARGS` on `push` to `main`. `test_workflows.py` pins exactly one
  writer (`test_exactly_one_job_holds_the_cache_write_credential` and a cluster around it).
- `CACHE_RC` (`--bazelrc=$RUNNER_TEMP/bazel-cache.rc` iff the file exists) is a per-task var in six places
  (`bazel.taskfile.yml` test:unit + test:accept, `rust.taskfile.yml` ×2, `test/taskfile.yml` .build-binaries,
  `website/schema.taskfile.yml`). Missing at: `cross.taskfile.yml` build/cquery, `scripts/test_bundle.py`
  (`bazel()` seam), `bazel.taskfile.yml` 104/177/194/217/241/260/274/445-446/692/798, `rust.taskfile.yml:446`,
  `test/taskfile.yml:560`, `website/{reference,}taskfile.yml`. `--bazelrc` is a startup option: two calls in one
  job that differ in it restart the server (the `cquery` after `test:unit` at 445-446 does).
- `test_workflows.py`'s `_BAZEL` matcher keys on the literal word `bazel`, so `task cross:*`, `schema:generate`
  and `rust:build` lanes are invisible to the cache sweeps.
- Bazel rc order: workspace `.bazelrc`, then `~/.bazelrc`, then `--bazelrc` files; a later file wins for the same
  command, so a `build --remote_upload_local_results=true` line in the CI rc overrides `.bazelrc:119`.
- Fork PRs receive no secrets: an empty `write-auth` writes no rc, and the lane reads anonymously.
- Nothing parses `remote cache hit`; Bazel's `INFO: N processes: M remote cache hit, …` line and
  `gh run view --json jobs` timings are the measurement.

## Q2 Conventions baseline contract (finding 9)

- `ocx-sh/conventions` `docs/contract.md`, `MANIFEST.toml`, `scripts/baseline_check.py` at `origin/main`
  (`14ff523`; tags `v0.1.0`, `v0.1.1`). Owner branch `feat/docs-reports-artifact` is one commit ahead.
- Template-owned (drift-checked): `.copier-answers.yml`, `taskfiles/baseline.taskfile.yml`, `.pre-commit-config.yaml`,
  `cliff.toml`, `renovate.json`, `CLAUDE.md` (body `@AGENTS.md`), `verify-basic.yml`, `verify-deep.yml`,
  `claude.yml`, `deploy-site.yml` (if `site.deploy`), `.github/zizmor.yml`, five client-dir `.gitattributes`,
  `bazel/baseline.bazelrc`. Seeded once: `taskfile.yml`, `ocx.toml`, `grimoire.toml`, `AGENTS.md`, `.gitleaks.toml`,
  `.github/renovate-local.json`, `.bazelrc`, `MODULE.bazel`.
- `baseline_check.py`: root tasks `verify, ci:lint, ci:test, ci:build, ci:docs, ci:deep`; baseline tasks reachable
  (taskfile included flat); every `ocx.toml` ref exact `X.Y.Z`; baseline tools `task` (default), `prek gitleaks grim
  uv` (lint), `git-cliff` (release); group names ⊆ {lint,test,build,docs,release}; `ocx.lock` tracked; it does
  **not** require `[tools]` to hold `task` only. Grim: clients claude/codex/opencode, bundles docs-essentials,
  code-docs-essentials, `<lang>-essentials` per language. Bazel: exact `rules_ocx` dep, no `http_archive` naming an
  ocx tool. Site: `ci:docs` summary must mention lighthouse; `packageManager` pnpm equals the ocx pin.
- verify-basic template: jobs `lint` (ci:lint, lock:check incl. grim:check, baseline:check, secrets = gitleaks over
  full history, workflows:lint = actionlint + zizmor), `test` (ci:test), `build` (ci:build, Bazel cache write only on
  push main), `docs`, `basic-gate` (`if: always()`, jq all success|skipped). No repo-extension job.
- verify-deep template: cron `sha256(repo_name)[:4] % 60` at 03 UTC (ocx → `11 3 * * *`), dispatch, workflow_call,
  `pull_request: [labeled]` gated on `run-deep`; one `deep` job over `deep_os` running `task ci:deep`; `nightly-red`
  job (schedule only; open/update on failure, close on success). `deep_os` validator allows only
  ubuntu-latest/macos-latest/windows-latest (fleet repo www-setup asks for macos-26).
- Template `release:prepare`: refuses unless the latest scheduled/dispatched verify-deep on the default branch is
  green (branch-bound, not SHA-bound), calls optional `release:bump-files`, writes CHANGELOG via git-cliff, runs
  `verify`. ocx's own `release:prepare` (`taskfiles/release.taskfile.yml:216-259`) collides by name.
- `fleet.toml [repos.ocx]`: `adopted = false`, stale `site.dist = website/.vitepress/dist` (site is Astro now).
  Adopted today: `[repos.conventions]` and `[repos.ocx-website]`.
- Template `release:prepare` "never tags or pushes" (its own desc); conventions setup action inputs are
  `bazel-cache: off|read|write` (write only on a default-branch push), `cache-read-auth`, `cache-write-auth`.
- `adopt.sh` appends an existing `CLAUDE.md` body to `AGENTS.md` under "## Imported from CLAUDE.md", `git rm`s
  `removed_by_adoption` paths and overwrites `cliff.toml`, `renovate.json`, `.pre-commit-config.yaml`, `claude.yml`.
- Q7 `packageManager` check passes when the field is absent (`website/package.json` has none).
- ocx collisions: `CLAUDE.md` (large; `AGENTS.md` is a pointer today), `cliff.toml`, `renovate.json`,
  `.pre-commit-config.yaml`, `claude.yml`, `verify-basic.yml`, `verify-deep.yml`; `ocx.toml` floats majors and names
  the task runner `go-task`; grimoire lacks `shell-essentials`; no Lighthouse gate; no gitleaks/grim in `ocx.toml`.
- ocx project groups are not activated by the shell hook (guide § Groups: "daily development needs neither"), so
  dev tools stay in `[tools]`; the baseline tools are added to their groups beside them (identity is `(group, name)`).

## Q3 Acceptance sharding (finding 1)

- `scripts/accept_shard.py`: `--shard i/n` 1-based, `select = stems[i-1::n]`, formats stems/paths/labels; callers
  `bazel.taskfile.yml:554-555` (labels) and `test/taskfile.yml:381` (paths). `accept_floor.py --expect-shards`
  requires `shard-{i}-of-{N}.xml` for i in 1..N. Tests pin 1-based names.
- Bazel sharding: `TEST_TOTAL_SHARDS`/`TEST_SHARD_INDEX` 0-based; a sharded test must touch
  `TEST_SHARD_STATUS_FILE` or Bazel reports `LOCAL_TEST_PREREQ_UNMET`. The one-`sh_test`-per-module layout never sets
  `shard_count`, so Bazel selects *targets*, not pytest items: one selector serves both paths.

## Q4 Ephemeral zot (finding 14)

- `test/src/native_registries.py` start/stop via pidfiles, fixed ports 5000/5003/5004, no crash safety; tasks
  `test:services:native:start|stop`; CI steps in verify-basic smoke-acceptance and verify-deep acceptance(-native).
- zot `http.port: "0"` binds a kernel-chosen port and logs it at info level (`port is unspecified, listening on
  kernel chosen port`, field `port`); `test/zot-config.json` logs at `warn`. Readiness: `GET /v2/` → 200.
- xdist: a worker has `config.workerinput`; the controller's `pytest_configure` runs before xdist spawns workers,
  and workers inherit the controller's environment, so ports exported there reach every worker.
- Crash safety: POSIX — a watchdog child blocking on a pipe the controller holds kills zot on EOF (one mechanism for
  Linux and macOS; `preexec_fn` is unsafe with threads). Windows — Job Object with `KILL_ON_JOB_CLOSE` (0x2000) via
  ctypes, handle held by the controller.
- `OCX_INSECURE_REGISTRIES` is a static `localhost:{{.REGISTRY_PORT}}` in the test taskfile anchor and in
  `test/bazel.bzl` `_RUNNER`; the runner starts the stack under `acceptance-stack-<port>.lock`. ~53
  `localhost:500x` hits in 23 files, mostly docstrings; real reads in `golden/generate.py`, `test_bench_smoke.py`,
  `attestations.py`, `test_shell_reconcile.py`.

## Q5 Windows and platform (#631, #632, #633, finding 13)

- #631: `ocx_util` `fs.rs` `persist_with_retry` does not see the target; the callers (`persist_temp_file`,
  `…_if_absent`, `…_noclobber`, `attempt_noclobber`, `published_at`, `write_bytes_atomic`) do. `path.rs` rejects
  verbatim/UNC paths; `resolver.rs:556` forbids `\\?\` in exported env values. `TEST_TARGET_MAP.toml` `ocx_util_test`
  `cases = 285` must rise with new tests. The test once named `test_patch_test_composes_a_companion_onto_a_base`
  is `test_patch_test_composes` in `test_patch_smoke.py`.
- #632: `bin_scan.rs` refusal arm `(Verify, _) | (Auto, None) if !host_can_scan` refuses even an empty scope;
  exit 65 / slug `bin_scan_unsupported_host` stay (no contract ledger entry); `test_patch_smoke.py:59`
  `no_bin_scan=True` is the workaround; help text `package.rs:118-120` renders into the generated reference.
- #633: `sigstore_stack()` raises deliberately when no Docker; limits `SUITE_FLOOR 3683`, `SKIP_CEILING 255`,
  `XFAIL_CEILING 5` read by `suite_ceilings.py` / `accept_floor.py --limits-dir`. `macos-15-intel` and
  `windows-11-arm` are free public-repo runners; `macos-13` is retired.
- WSL→Windows: a Windows process cannot use a `\\wsl$` cwd, so the tree is staged on `C:`; env crosses via `WSLENV` or
  a `set` inside `cmd.exe /c`. `/mnt/c/Users/ecom/grim-wintest/run.sh` is the template (rsync tracked tree,
  `cmd.exe /c "set …&& uv sync && uv run pytest …"`, strip CR). `cross.taskfile.yml` builds gnullvm (`xwin.taskfile.yml`
  is MSVC, the wrong target here).

## Q6 Release (finding 15, #635)

- `release.yml` is cargo-dist 0.31.0-generated, tag-triggered only; drift-checked by `verify-release-ci.yml`
  (`dist generate --check --mode ci`). Custom job hooks: `plan-jobs` (a red plan job leaves builds skipped but host can
  still run — wrong site), `local-artifacts-jobs` (needs plan, sits in `host.needs`/`if` → blocks publish — the gate
  site), `global-artifacts-jobs` (blocks; scan-binaries lives here), `host-jobs` (does not block). Custom job contract:
  `workflow_call` with string input `plan`; `github-custom-job-permissions` replaces the defaults.
- `release-readiness.yml`: dispatch, no inputs; calls verify-deep + shell-activation-deep; stale header 7-14, 54-55.
- Query: `gh run list --workflow release-readiness.yml --commit <sha> --status success --limit 1
  --json databaseId,headSha,conclusion,url,displayTitle` (needs `actions: read`).
- `__testing` hooks: 17 `Testing` vars in `ocx_env/src/vars.rs:512-547`; ~17 acceptance modules read them
  (test_install_libc 28 uses, test_self_setup 12, test_project_crash_recovery 7, test_transport_git 7,
  test_execution_records 6, test_logging 5, test_managed_config 5, test_self_update 4, test_extra_ca_certs 4,
  test_package_claim 3, test_lock 2, test_update_config 2, test_update_check_throttle 2,
  test_index_servable_snapshot 2, test_login 1, test_cascade 1, test_announce 1) plus helpers. No release-binary
  marker exists.
- #635 stale text: `release_provenance_check.py:31-32`, `release-readiness.yml:8-9,54`, `verify-deep.yml:237-244`
  (accurate, over the comment cap), `rust.taskfile.yml:85-86,147-152,616,644,865-871`, ADR rules_rs 102, 111.

## Q7 Lore and #634

- `ocx-sh/grimoire-lore`: `rules/bazel-quality/rust.md` (crate_universe only, 0 rules_rs mentions),
  `skills/bazel-adopt/references/branch-rust.md`, `rules/bazel-quality/caching.md` BZL-CACHE-02 ("more than one
  un-gated actor is the finding"); `publish.toml` bazel-quality 0.4.1, bazel-adopt 0.1.1, bazel-diagnose 0.1.1, bundle
  bazel-essentials 0.1.3; `publish.yml` on push to main publishes and announces. Local checkout sits on owner branch
  `hex/ocx-project-baseline` (7 commits ahead).
- #634 item 2: the doctest-stdout patch reverts hermeticbuild/rules_rust#87, which added
  `--discard-stdout-on-success` on purpose (completion-order output broke the replay cache key); upstreaming the
  removal would be rejected. Dropping doctest counting (exit code only) removes the patch, `MODULE.bazel:43-47`, the
  22 doc rows in `TEST_TARGET_MAP.toml` and their proofs.
