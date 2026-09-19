# Research: verification-tier mechanics for the crate-split ADR (CI triggers, satellite build, pytest smoke tier, stamp scope)

## Metadata

**Date:** 2026-09-16
**Domain:** ci-cd | testing | devops
**Triggered by:** `adr_crate_split_workspace.md` § "Verification tiers" — the smoke tier, the scoped WP gate, `verify-deep.yml`'s draft-gated `pull_request` + `merge_group` trigger (Ruling OQ1), the `.verify:mark` scope field
**Expires:** 2027-03-16

Extends `research_crate_split_operability.md` (AI-config/CI-cost angle) — this artifact does not repeat its findings; it is the mechanics layer underneath the ADR's already-ratified design.

## Direct Answer

Every one of the ADR's nine open mechanics questions has a decisive, low-risk answer that needs **no new dependency and no new tool**: the draft-gate + `merge_group` trigger is a documented (if easy to get wrong) YAML shape; the satellite-verify job's non-blocking behaviour is a plain job-level `continue-on-error: true` with a one-line flip; the satellite build is a source overlay into the submodule checkout, not a `[patch]` trick, because Cargo cannot patch across a git-repo boundary (already established in the ADR); the smoke tier's wall-clock budget must live in the taskfile, not in a `pytest_sessionfinish` hook, because pytest-xdist's controller-side `sessionfinish` is documented as unreliable relative to worker completion; the anti-rot command-surface check should keep parsing `command.rs` as source text (the ADR's own already-chosen mechanism), not reach for a clap-introspection crate, because none ships in mainline `clap`; the `paths:` glob-liveness check is a straight copy of the pattern this repo already has for rule files, because neither `actionlint` nor `zizmor` do this; and the ≤300s/≤60s budgets must be asserted in a task's `cmds:`, never its `preconditions:`, because `task --force` — which `task verify:scoped` needs for cache-bypass anyway — is documented to skip preconditions but not to skip cmds' exit codes.

## Q1 — Draft-gated `pull_request` + `merge_group` on `verify-deep.yml`

**Recommendation: ship exactly the ADR's ruling, with `ready_for_review` in `types:` — its omission is the single most common way this trigger silently never re-fires.**

```yaml
on:
  pull_request:
    branches: [main]
    types: [opened, synchronize, reopened, ready_for_review]
  merge_group: {}

jobs:
  build:
    if: github.event_name != 'pull_request' || github.event.pull_request.draft == false
    # ...
```

- **The `ready_for_review` pitfall is real and undocumented as a footgun.** GitHub's default `pull_request` activity types are `opened`, `synchronize`, `reopened` only — `ready_for_review` is not among them, and without it in `types:` the workflow **never fires at all** when a draft is converted to ready (not merely blocked by the job-level `if:`) — a materially different failure than "runs but skipped." [Events that trigger workflows](https://docs.github.com/en/actions/using-workflows/events-that-trigger-workflows) states the default-three explicitly and that `ready_for_review` must be opted into via `types:`. This is still an open pain point, not something GitHub has since defaulted-on: [community discussion #139644](https://github.com/orgs/community/discussions/139644), "Pull request default activity types should include `ready_for_review`," remains open with no product change landed as of this research.
- **`github.event.pull_request.draft` is absent (not `false`) on `merge_group` events** — there is no `pull_request` object on that event at all, so the naive guard `github.event.pull_request.draft == false` alone would evaluate to `false` on a `merge_group` run and skip it. The ADR's actual ruling — `github.event_name != 'pull_request' || github.event.pull_request.draft == false` — is correct precisely because it short-circuits on event name first; keep that clause order.
- **Required-check name matching**: the check name shown to branch protection is the job's rendered name, and it must be byte-identical between the `pull_request` run and the `merge_group` run for GitHub to treat them as the same required check — [Managing a merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue) states plainly that a required workflow missing the `merge_group` trigger stalls the queue rather than erroring. Today's ruleset (id 13466314) names no required checks, so this is moot until one is added — consistent with the ADR's own note; the residual is that a `strategy.matrix`-derived job name (interpolated values) must render identically on both events, which it will as long as the matrix inputs don't reference `github.event.pull_request.*`.
- **`concurrency:` group keys for both events**: `github.head_ref` is only set on `pull_request` events and is empty on `merge_group`, `push`, and `workflow_dispatch`, so a bare `${{ github.head_ref }}` key silently degrades to a workflow-wide single-slot lock on non-PR events. Worse, GitHub's merge queue and Actions concurrency actively fight when a later merge-queue entry cancels an in-flight one — [runs-on.com's concurrency guide](https://runs-on.com/github-actions/concurrency/) documents this as a still-current gotcha, not a 2025/2026 regression. The working pattern distinguishes the merge-queue ref explicitly:
  ```yaml
  concurrency:
    group: verify-deep-${{ (startsWith(github.ref, 'refs/heads/gh-readonly-queue/') && github.run_id) || github.event.pull_request.number || github.ref }}
    cancel-in-progress: ${{ !startsWith(github.ref, 'refs/heads/gh-readonly-queue/') }}
  ```
  `verify-deep.yml` today uses a **literal** `verify-deep-${{ github.ref }}` group (not the `${{ github.workflow }}` convention) because `release-readiness.yml` also composes it via `workflow_call` — keep that literal prefix, just widen the key expression above it.
- **Cost / double-run**: the ADR adds only `pull_request` + `merge_group`, not `push`, to `verify-deep.yml`, so there is no push-vs-PR double-fire to dedupe — `verify-basic.yml` already owns the `push: branches: [main]` lane. No action needed here; flagging it only because the question asked and it is the standard dedupe pattern (`push` scoped to `main` only, PR-time coverage carried entirely by `pull_request`).
- **`paths-ignore` interaction — the "expected, waiting forever" trap is real and confirmed.** [Troubleshooting required status checks](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/collaborating-on-repositories-with-code-quality-features/troubleshooting-required-status-checks) states plainly: "Avoid requiring workflows that can be skipped" — a path-filtered required workflow that never fires leaves the check "Expected — Waiting for status to be reported" forever, blocking merge. Confirmed independently by multiple community threads (e.g. [#113714](https://github.com/orgs/community/discussions/113714), [#49124](https://github.com/orgs/community/discussions/49124)). `verify-deep.yml` today has **zero `paths:` filters** (confirmed in the tooling inventory) — keep it that way. If path-scoping is ever wanted, the documented workaround is not `paths-ignore` on the trigger but an always-running gate job (e.g. `dorny/paths-filter`-computed condition) that reports its context on every run and only does the expensive work conditionally — never a trigger-level filter on a required workflow.

## Q2 — Non-blocking `task satellite:verify` in `verify-deep.yml`

**Recommendation: job-level `continue-on-error: true`, hardcoded (no repo variable). Flip = delete that one line once the mirror's manifest depends on the new crate names.**

```yaml
  satellite-verify:
    needs: [build]
    continue-on-error: true   # non-blocking until ocx-mirror's Cargo.toml names the split crates — remove this line then
    runs-on: ubuntu-latest
    steps:
      - run: task satellite:verify
```

- **Step-level `continue-on-error` is the wrong shape**: it makes the job show an all-green success bubble even when the step actually failed, and the only way to notice is opening step details — [Ken Muse, "How to Handle Step and Job Errors"](https://www.kenmuse.com/blog/how-to-handle-step-and-job-errors-in-github-actions/) and [community discussion #15452](https://github.com/orgs/community/discussions/15452) both confirm this is a known, still-unfixed UI blind spot. That defeats "visible."
- **Job-level `continue-on-error: true` is the right shape**: per the same sources, a failing step inside such a job still shows a **red badge on the job** in the Actions run view (visible), while the job's reported **conclusion to any consumer (required-check logic, `needs:` gating) is `success`** (non-blocking). This is exactly "visible but non-blocking."
- **Rejected alternatives**: a `workflow_dispatch`-only sibling workflow would not run automatically per-PR, defeating visibility entirely. A repo-variable gate (`if: vars.SATELLITE_VERIFY_BLOCKING == 'true'` driving `continue-on-error: ${{ vars.SATELLITE_VERIFY_BLOCKING != 'true' }}`) would let the flip happen via repo settings with no commit — genuinely nicer operationally — but it is a second moving part (a variable whose default-unset behaviour must itself be tested) for a flip that is, in practice, a single planned commit landing alongside the mirror-side migration anyway. Ponytail call: the hardcoded line is the one-rung-lower answer and is not worse for this use case; note the variable form as the upgrade path if the flip date turns out to be operator-driven rather than commit-driven.
- **The exact flip needed later**: remove the `continue-on-error: true` line (or set it to `false`) in the same commit that lands `ocx-mirror`'s manifest changes to depend on the split crate names — see Q3, where the structural reason the job is *expected* red until then is explained.

## Q3 — Building `ocx-mirror`'s submodule against this workspace's tree

**Recommendation: option (b) — overlay this PR's tree into the checked-out submodule, then build. Option (a) tests the wrong commit; option (c) cannot work at all.**

```sh
# in a job that has both repos checked out (actions/checkout for ocx-mirror, path: mirror)
git -C mirror submodule update --init --recursive external/ocx   # bring in the pinned pointer's tree first
rsync -a --delete --exclude='.git' "$GITHUB_WORKSPACE"/ mirror/external/ocx/   # overlay THIS PR's source
cd mirror
cargo build --workspace --locked
```

- **Why not (a)**: checking out `ocx-mirror` at its pinned pointer and building it tests the commit already recorded at that pointer — which is what "ocx-mirror builds at the matching pointer" already checks today, and which the ADR itself calls "a checkbox, not a gate." It structurally cannot catch a break this PR introduces, because this PR's tree is never present in that build.
- **Why not (c)**: a `[patch]` override cannot cross the git-submodule boundary — this is not new research, it restates the ADR's own already-cited constraint that `[patch.crates-io]` and any path-based override resolve inside the *consumer's* graph and do not travel across a path/git dependency boundary, and that Cargo cannot resolve a transitive path dependency across two separate git repositories ([rust-lang/cargo#14946](https://github.com/rust-lang/cargo/issues/14946), already cited in the ADR). The mirror vendors `ocx` as a full-tree git submodule specifically because this limitation exists; a `[patch]`-based `satellite:verify` would fight the exact mechanism the ecosystem-contract section already locked in.
- **Cargo.lock handling — the load-bearing subtlety**: path dependencies do not need a `Cargo.lock` bump to pick up new source content (their identity in the lock is the path + version string, not a registry checksum), so the `rsync` overlay alone is sufficient for the *build* to see this PR's code. What **does** need a change is `ocx-mirror`'s own `Cargo.toml` `[dependencies]` table: today it names `ocx_lib`; after the split it must name the specific `ocx_*` crates it actually uses. **This is the real reason the job must stay non-blocking (Q2) until the mirror pointer bump lands**: `satellite:verify` will fail structurally — "package not found," not a real regression — for every PR between the day this job ships and the day `ocx-mirror`'s manifest is migrated to the new crate names. That migration, and the pointer bump, are the trigger for the Q2 flip.
- **Runtime cost — estimate, not measured**: no build-time or LOC figure for `ocx-mirror` was fetched in this pass (out of scope: this is a sibling private-adjacent repo, not something to clone speculatively for a timing run). Order-of-magnitude estimate only: `ocx_lib`'s own cold build is 71.7 s (`research_crate_split_performance.md`, measurement 1); a mirror-sized consumer crate on top is plausibly another 20–40 s, plus checkout/submodule-init overhead. Recommend a `Cargo.lock`-hash-keyed `actions/cache` on the mirror's `target/` directory so this cost is paid once per lockfile change, not per PR, and recommend phase-0 record the real number the first time the job runs rather than trusting this estimate — this is exactly the "unchecked green" trap `quality-core.md` warns about if left unmeasured indefinitely.

## Q4 — Pytest smoke tier mechanics

**Recommendation: register the marker with `--strict-markers`; keep `--dist loadgroup` for the smoke run too; enforce the wall-clock budget in the taskfile shell wrapper, never in a `pytest_sessionfinish` hook; keep the anti-rot command-surface check as a source-text scan of `command.rs`, not a clap-introspection dependency.**

- **Marker registration**: add to `test/pyproject.toml`'s existing `markers = [...]` list:
  ```toml
  markers = [
      "requires_tty: ...",
      "divergence: ...",
      "smoke: fast, curated per-verb happy-path subset; zero sign/attest/verify/cosign coverage — see test_smoke_coverage.py",
  ]
  ```
  and add `addopts = "--strict-markers"` to `[tool.pytest.ini_options]` — **not present today** (confirmed: no `addopts` key exists in the current 72-line file), so a typo'd `@pytest.mark.smoke` currently fails silently rather than erroring at collection.
- **Selection**: `uv run pytest -m smoke -n auto --dist loadgroup`. Keep `--dist loadgroup` (not `--dist load`) even though the smoke set need not itself contain an `xdist_group`-marked test: [pytest-xdist's distribution docs](https://pytest-xdist.readthedocs.io/en/latest/distribution.html) describe `loadgroup` as behaving exactly like `load` for any test carrying no group mark, so there is no cost to keeping the same dist mode the full suite already uses, and it means a future smoke test that does need grouping (e.g. one touching the same global patch-descriptor slot as `test_patches.py`) is safe by default rather than by remembering to add the flag later.
- **Wall-clock budget: do not use `pytest_sessionfinish` under xdist.** [pytest-xdist issue #1045](https://github.com/pytest-dev/pytest-xdist/issues/1045) documents that the controller's `pytest_sessionfinish` fires while workers may still be finishing/tearing down — i.e., xdist does not guarantee this hook runs "right before returning the exit status," which is pytest's own documented contract for the hook in a non-distributed run. A budget check that reads elapsed time in that hook is measuring an unreliable clock. It is also structurally the wrong layer regardless of that bug: a `conftest.py`-defined fixture or hook runs **per worker process** under xdist, so a naive `time.monotonic()` diff inside a fixture never sees the other workers' time at all — only a controller-side hook could even attempt it, and that is the one shown unreliable. **Recommendation**: measure wall time in the taskfile invocation itself, outside pytest entirely (see Q8's exact pattern) — reliable regardless of worker count, and inherently CI-visible as a task failure rather than a buried pytest warning.
- **`--durations=N`**: add `--durations=20` (or `=0` for the full list) to the smoke invocation as supporting evidence for the budget and for spotting a slow test before it needs to be demoted out of the smoke set — a standard, uncontroversial pytest flag, no citation needed beyond the tool's own `--help`.
- **Anti-rot command-surface check — no clap-introspection dependency needed.** Mainline `clap`/`clap_complete` (the crates already in `ocx_cli`'s dependency tree) expose no stable JSON command-tree export; the tools that do this (`clap_schema`, `clap_describe`, `brontes`) are all third-party crates built specifically to fill that gap, none vendored here. Per this repo's own "Don't Own Non-Domain Code" bar and YAGNI, adding one for a single anti-rot test is not justified when the ADR's own phase-0.6 deliverable (`test_smoke_coverage.py`) already chose the cheaper, dependency-free mechanism: parse `crates/ocx_cli/src/command.rs` as source text (regex over `pub mod \w+` / `#[derive(Subcommand)]` variant names) to enumerate the ~20–25 top-level verbs, then cross-reference against the smoke tests' invoked CLI arguments. This sidesteps the "does clap expose machine-readable output" question entirely — recommend keeping that design rather than switching to a built-binary `--help`-parsing or `clap_complete`-generation approach, both of which require a built binary as a precondition and are fragile to clap's exact human-oriented rendering.
- **Zero-signing-coverage check — static scan, not a nested pytest collect-only run.** Recommend a plain-Python regex scan over `test/tests/*.py` for `@pytest.mark.smoke` decorated function names, asserting none match `re.search(r"sign|attest|verify|cosign", name, re.I)`, rather than shelling out to `pytest -m smoke --collect-only -q` and grepping nodeids. Nested pytest invocations (a test that itself runs pytest) are slower, and under `-n auto --dist loadgroup` the outer run's worker processes each spawning a nested pytest collection is needless overhead for what is fundamentally a source-text property. This mirrors the anti-rot check's own reasoning above and needs no new tooling.

## Q5 — Pre-commit verification stamp schema

**Recommendation: `{"timestamp": <epoch:int>, "scope": "full"|"scoped", "crates": [<name>, ...]}`, fail-closed on any parse error, two red-state tests.**

```python
def is_recently_verified(self, ttl_seconds: int = 300, require_full: bool = False) -> bool:
    verify_file = self.state_dir / "commit-verified"
    if not verify_file.exists():
        return False
    try:
        data = json.loads(verify_file.read_text())
        timestamp = data["timestamp"]
        scope = data["scope"]
    except (json.JSONDecodeError, KeyError, ValueError, OSError):
        return False  # fail-closed: unparseable (including the old bare-int format) is "not verified"
    if require_full and scope != "full":
        return False
    return (time.time() - timestamp) < ttl_seconds
```

- **No compatibility shim for the old bare-int format** — an old-format file hits `json.JSONDecodeError` and returns `False`, which is correct: this state file is local hook plumbing, not an interface per `CLAUDE.md`'s stability tiers, and a stale file simply re-triggers verification rather than silently trusting an unscoped stamp. This is also the concrete instance of `quality-core.md`'s "degrade-to-`Ok` erases the refusal" anti-pattern to avoid: any parse ambiguity must resolve to *not verified*, never to *verified*.
- `build_deny_reason`'s hint text (`echo $(date +%s) > .../commit-verified`) must change in the same commit — it currently teaches the exact bare-int format this switch makes invalid; the new hint is `task verify:mark` (which already exists as the public wrapper task) rather than a hand-typed `echo`, since the JSON shape is no longer hand-typeable.
- **Prior art, briefly**: pre-commit's `--hook-stage` selects **which hooks run at which git lifecycle point** (commit/push/manual) — a routing mechanism, not a persisted verification-scope record consumed later. `lefthook`'s `skip`/`only` conditions (branch, tags, merge-in-progress) are the same category: they decide whether a hook runs, not what a downstream reader trusts about a prior run. Neither tool ships anything resembling a scoped provenance stamp read by a *later, different* command (`/hex-finalize`) — this is closer in spirit to a CI artifact-metadata record than to either framework's hook-selection config, which is why it stays hand-rolled (no library implements this narrow a contract — satisfies `quality-core.md`'s bar for owning non-domain code).
- **Two red-state tests**: (1) write `{"timestamp": time.time(), "scope": "scoped", "crates": ["ocx_util"]}`, call `is_recently_verified(300, require_full=True)`, assert `False` — a scoped mark must not satisfy a full-required check. (2) write the literal string `"1757960000"` (today's bare-int format, or any truncated JSON) to the same path, call `is_recently_verified(300, require_full=False)`, assert `False` — fail-closed on unparseable input, not a silent pass-through.

## Q6 — Scoped-gate escalation rules vs. practice

**Recommendation: keep the ADR's rule as written; add `cargo check --workspace --locked` as a cheap universal safety net beneath the per-crate `clippy -p`/`nextest -p` steps — this closes a real gap the ADR's hub-threshold (≥ 4 reverse-deps) does not.**

- **rust-lang/rust practice**: `x.py test` subsets by stage/component for local iteration, but the authoritative merge-time signal is still the full bors/merge-bot suite — the same two-tier shape (cheap local subset, full authoritative gate) the ADR already claims analogy to. This is well-documented, longstanding practice ([Rust Forge, "Bors"](https://forge.rust-lang.org/infra/docs/bors.html)) and is not contradicted by anything found in this pass (see Q9 — rust-lang/rust has not migrated off its custom bot as of January 2026).
- **The documented false-negative record of predictive selection** — already established in `research_crate_split_prior_art.md` (bazel-diff, turborepo #9234) — is the reason the ADR chose a mechanical, path-resolved rule over an impact-analysis tool. One-line pointer only, per the prompt; not re-researched here.
- **Residual the ADR accepts, verbatim from the ADR itself**: "A change confined to one non-hub internal crate can still alter the single `ocx` binary's black-box behaviour in a command the glob table does not map to that crate... the same trade rust-lang/rust makes between its per-push subset and its merge-time full suite." That residual is about **behavioural** drift and is correctly accepted — no local gate can cheaply prove black-box CLI behaviour for every command on every touch.
- **A different, uncalled-out residual exists at the compile layer**: `cargo clippy -p <crate>` and `cargo nextest run -p <crate>` build *only* the named crate (plus its own dependencies), never its reverse dependents. For a crate with 1–3 reverse-dependents (below the hub threshold of ≥ 4, so it does **not** escalate to the full gate), a changed public signature that breaks a caller in one of those 1–3 dependents produces a flat **compile error** — not a subtle behavioural drift, an outright build break — that the scoped gate as specified would not catch at all, because it never builds the dependent crates. This is not the same residual the ADR names (which is behavioural); it is a build-breaking one, and `cargo check --workspace` is the cheap, well-understood fix: Cargo's own incremental fingerprinting means a workspace-wide `check` after a warm target directory only recompiles the touched crate plus whatever transitively depends on its changed public interface — i.e., exactly the reverse-dependents a targeted `-p` build skips, at close to zero marginal cost for everything else. Recommend adding `cargo check --workspace --locked` (not `clippy --workspace`, which is more expensive per-crate and duplicates the already-run `clippy -p`) as an unconditional step in `task verify:scoped`, ahead of the per-crate clippy/nextest steps.
- **No published number found** for the `check --workspace` (warm cache) vs `check -p` wall-clock delta on a workspace this shape — flagging as an open measurement rather than inventing one; recommend phase-0's `task verify:scoped` timing pass (already planned) record this delta against the 5-minute budget rather than trusting an estimate.
- **uv/ruff-style `paths:`-filtered per-crate CI** is a real, common pattern in Rust-monorepo CI generally (e.g. via `dorny/paths-filter`-computed job conditions), but no specific astral-sh workflow file was fetched and verified in this pass — stating the general pattern only, not a claim about uv or ruff's actual CI, per `quality-core.md`'s verification-honesty rule (no fabricated specifics).

## Q7 — Workflow `paths:` glob-liveness check

**Recommendation: a sibling pytest in `.claude/tests/test_ai_config.py`, copying the exact mechanism already used for `.claude/rules/*.md` — `actionlint` and `zizmor` both confirmed out of scope for this.**

- **Confirmed neither tool does this.** `actionlint`'s documented checks are syntax, expression/context typing, shellcheck integration, and action-input validation against the referenced action's own metadata — none of it is "does this glob match at least one file in the working tree," which requires walking the repo's actual file list, something `actionlint` (scoped by default to `.github/workflows/` only) does not do. `zizmor`'s documented audits are security-posture checks (template injection, excessive permissions, unpinned refs, cache poisoning) — also not glob-liveness. This matches this repo's own `test_ai_config.py` prior art existing specifically *because* no generic linter covers it.
- **Implementation**: add `test_workflow_paths_globs_match_files` beside the existing `TestRuleGlobs::test_all_rule_globs_match_files` (`.claude/tests/test_ai_config.py:348`), reusing its `glob.glob(str(ROOT / pattern), recursive=True)` check, but sourcing patterns via PyYAML over `.github/workflows/*.yml`'s `on.push.paths`, `on.pull_request.paths`, and `paths-ignore` keys instead of rule frontmatter.
- **Glob semantics gotchas, load-bearing**:
  - GitHub's `paths:`/`paths-ignore:` dialect supports a **leading `!`** to negate a prior positive match within the same list — Python's `glob.glob` has no negation concept at all. Feeding a `!crates/**` entry straight into `glob.glob` either errors or (more likely) returns zero matches for the literal string `!crates/**`, producing a **false** dead-glob failure on a pattern that is semantically fine. The test must strip/skip `!`-prefixed entries (checking only that the *un-negated* tail still matches something, or excluding negations from liveness checking altogether, since "this must NOT match" is a different assertion than "this must match ≥ 1 file").
  - `**` behaves the same way in both dialects (matches across directory boundaries) — no special-casing needed there, matching how the existing rule-glob test already handles `crates/**/*.rs`-shaped patterns.
  - Path matching is repo-root-relative and forward-slash in both dialects on Linux runners, so `ROOT / pattern` composition (as the existing test already does) needs no adjustment.

## Q8 — Making the ≤ 5 min / ≤ 60 s budgets reproducible and un-bypassable

**Recommendation: assert the budget in the task's `cmds:`, never its `preconditions:` — confirmed that `task --force` skips `preconditions:` but does not skip a `cmds:` step's exit code.**

- [Task's own usage docs](https://taskfile.dev/usage/) state directly: "a task executed with a failing precondition will not run unless `--force` is given" — i.e., `--force` is specifically documented to **bypass** `preconditions:` checks, which is exactly the failure mode this repo's own memory flags (`task --force verify` is the documented way to bypass `sources:`/`status:` caching for a full run) and which would make a budget assertion placed in `preconditions:` silently vanish under the same flag that legitimately needs to be used for cache-busting. The same docs describe `--force`'s actual job as forcing a task to run "even when up-to-date" (i.e., it overrides the `status:`/`sources:` skip decision) — nothing in that description touches whether a `cmds:` step's own exit code is honored once the task does run.
- **Exact pattern** — a `cmds:` step wrapping the real invocation in a `time`-and-assert shell snippet, placed as the task's own final command so it is never in a position a caching directive could skip:
  ```yaml
  test:smoke:
    desc: Fast curated smoke tier — must stay under 60s wall.
    cmds:
      - |
        start=$SECONDS
        uv run pytest -m smoke -n auto --dist loadgroup --durations=20
        elapsed=$((SECONDS - start))
        echo "smoke tier: ${elapsed}s (budget 60s)"
        test "$elapsed" -le 60
    dir: test
  ```
  Identically shaped for `verify:scoped`'s 300 s budget. This is reproducible (same shell arithmetic every run, local or CI), CI-visible (a non-zero exit from `test "$elapsed" -le N` fails the task/job outright, no separate reporting step needed), and immune to `--force` because `--force` only changes whether the `cmds:` block runs at all, never how its exit code is interpreted once it does.

## Q9 — Trend scouting

- **Merge queue adoption is present but not yet dominant among flagship OSS Rust projects.** Per [Inside Rust, Q4 2025 recap / Q1 2026 plan](https://blog.rust-lang.org/inside-rust/2026/01/13/infrastructure-team-q4-2025-recap-and-q1-2026-plan/), rust-lang/rust as of January 2026 has only just "completed the migration off Homu" onto a **new custom bors bot** — GitHub Rulesets and native-merge-queue-as-IaC are named as a **Q1 2026 aspiration**, not a shipped state. Smaller Rust projects have adopted GitHub's native merge queue directly (e.g., a December 2025 switch reported for meilisearch-rust — single secondary-source citation, lower confidence, worth independently confirming before citing further). Net: the ADR's own framing — "Enabling the merge queue stays an owner repo-settings action... no longer a prerequisite" — is the right level of urgency; this is not a "everyone already did this" trend yet, even at the top of the Rust OSS ecosystem.
- **`merge_group` × Actions-concurrency interaction remains a live, still-current gotcha** (cancelling an in-flight `merge_group` run can break the queue) — not a new 2025/2026 regression, just still unfixed and worth the explicit concurrency-key carve-out in Q1.
- **pytest 9.0 landed November 2025 (patch 9.0.2 in April 2026)**: adds native TOML config support and subtests, removes every API that carried a `PytestRemovedIn9Warning` in 8.x (nose-style setup/teardown, the legacy `pytest.collect` namespace, yield-based tests) — none of this touches marker registration, `-m` selection, or `pytest-xdist` compatibility in a way that affects the smoke-tier design above. No pytest-9-specific breaking change relevant to markers or xdist was found in this pass; flagging the absence rather than inventing a concern.
- **No GitHub Actions change to `pull_request` draft-event defaults in 2025–2026 was found.** The `ready_for_review`-must-be-explicit trap (Q1) remains exactly as it has been; [community discussion #139644](https://github.com/orgs/community/discussions/139644) requesting the default be widened is still open with no resolution.

## Sources

| Source | Type | Date fetched | Relevance |
|---|---|---|---|
| [Events that trigger workflows](https://docs.github.com/en/actions/using-workflows/events-that-trigger-workflows) | Docs | 2026-09-16 | Q1 default `pull_request` types, `merge_group` activity type |
| [Managing a merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue) | Docs | 2026-09-16 | Q1 required-workflow/`merge_group` requirement |
| [Troubleshooting required status checks](https://docs.github.com/en/pull-requests/collaborating-with-pull-requests/collaborating-on-repositories-with-code-quality-features/troubleshooting-required-status-checks) | Docs | 2026-09-16 | Q1 `paths-ignore` "expected forever" trap |
| [community #139644](https://github.com/orgs/community/discussions/139644) | Discussion | 2026-09-16 | Q1/Q9 `ready_for_review` default-types gap, still open |
| [community #113714](https://github.com/orgs/community/discussions/113714), [#49124](https://github.com/orgs/community/discussions/49124) | Discussion | 2026-09-16 | Q1 `paths-ignore` deadlock, confirmed pattern |
| [runs-on.com concurrency guide](https://runs-on.com/github-actions/concurrency/) | Blog | 2026-09-16 | Q1/Q9 concurrency-key shape for `merge_group` + `pull_request` |
| [Ken Muse — step/job error handling](https://www.kenmuse.com/blog/how-to-handle-step-and-job-errors-in-github-actions/) | Blog | 2026-09-16 | Q2 `continue-on-error` job vs step UI behaviour |
| [community #15452](https://github.com/orgs/community/discussions/15452) | Discussion | 2026-09-16 | Q2 `continue-on-error` UI visibility gap |
| [rust-lang/cargo#14946](https://github.com/rust-lang/cargo/issues/14946) | Issue | (already cited in ADR) | Q3 cross-repo path-dependency limitation |
| [pytest-xdist distribution docs](https://pytest-xdist.readthedocs.io/en/latest/distribution.html) | Docs | 2026-09-16 | Q4 `--dist loadgroup` behaviour with/without group marks |
| [pytest-xdist issue #1045](https://github.com/pytest-dev/pytest-xdist/issues/1045) | Issue | 2026-09-16 | Q4 `pytest_sessionfinish` unreliable relative to worker completion |
| clap_schema, clap_describe, brontes (crates.io/GitHub search results) | Repos | 2026-09-16 | Q4 confirms no JSON command-tree export in mainline clap |
| [Taskfile usage docs](https://taskfile.dev/usage/) | Docs | 2026-09-16 | Q8 `--force` bypasses `preconditions:`/`status:`, not `cmds:` exit codes |
| [Inside Rust, Q4 2025/Q1 2026 infra recap](https://blog.rust-lang.org/inside-rust/2026/01/13/infrastructure-team-q4-2025-recap-and-q1-2026-plan/) | Blog | 2026-09-16 | Q6/Q9 rust-lang/rust still on custom bors bot, merge-queue-as-IaC not yet shipped |
| [Rust Forge — Bors](https://forge.rust-lang.org/infra/docs/bors.html) | Docs | 2026-09-16 | Q6 two-tier PR-subset/merge-time-full pattern |
| [pytest 9.0.0 announcement](https://docs.pytest.org/en/stable/announce/release-9.0.0.html) | Docs | 2026-09-16 | Q9 pytest 9 changes, none marker/xdist-breaking |
| `.tmp/hex/plan-crate-split/discover_tooling.md` §§ 1–3, 9 | Internal | 2026-09-16 | Current taskfile/pytest/CI-timing baseline this research extends |
