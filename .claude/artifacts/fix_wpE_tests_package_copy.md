# WP-E (tests) — review-fix log for `ocx package copy`

Branch: `hex/pkgcopy-fix--tests` (worktree `.agents/worktrees/tests`), based on
`evelynn` @ `dfcdcb98`. Findings owned per `review_r1_testcov_package_copy.md`
(and the shared items from `review_r1_spec_package_copy.md`), in the priority
order the task assigned. File set touched: `test/conftest.py`,
`test/tests/test_package_copy.py`, `test/tests/test_registry_startup_retry.py`
(new — a Docker-free unit test for the fix below; not in the originally
enumerated set, added because finding 1 needed a real, executed red/green
pair and the acceptance suite alone cannot give one without Docker).

## Findings

| # | Severity | Finding | Status | Where |
|---|---|---|---|---|
| 1 | BLOCK | `target_registry` skipped instead of retrying a cold-start race | fixed | `test/conftest.py` (`_wait_for_reachable` + `pytest_sessionstart`), `test/tests/test_registry_startup_retry.py` |
| 2 | HIGH | `--canonical-tag` default-on, untested | fixed | `test_canonical_tag_default_writes_a_digest_named_tag`, `test_no_canonical_tag_suppresses_it` |
| 3 | HIGH | two-phase write order has no acceptance test | fixed | `test_dry_run_leaves_the_targets_existing_index_byte_for_byte_unchanged` |
| 4 | HIGH | documented exit 64 vs actual exit 65 for "no platform in source matches `--platform`" | fixed — decided by team-lead: 64 correct, RED until WP-D | `test_a_platform_the_source_does_not_offer_is_a_usage_error`, `test_a_platform_typo_names_the_platform_not_the_manifest` |
| 5 | (convergence gaps) | #1 same-registry/different-repo multi-platform; #6 second copy after new source platform; #9 `--offline` → 81 | fixed | `test_copy_within_the_same_registry_to_a_different_repository`, `test_a_second_copy_after_the_source_gains_a_platform_adds_only_the_new_one`, `test_offline_refuses_before_touching_the_network` |
| 6 | WARN | tolerance band, bare `except Exception`, missing positive controls, missing stream assertions | fixed | see per-item breakdown below |
| 7 | SUGGEST | idempotency positive control; setups.py/docker-compose.yml note | fixed / no-change-needed | `test_a_repeated_copy_reports_unchanged`; docker-compose note below |

### Finding 1 — target_registry retry-race (BLOCK)

**Fix.** `test/conftest.py`: extracted the ad-hoc `mirror_registry` retry loop
into a pure, dependency-injectable helper `_wait_for_reachable(is_reachable,
*, attempts=10, delay_seconds=0.5, sleep=time.sleep)`, and called it for
*both* `mirror_registry` and `target_registry` in `pytest_sessionstart` (the
original code only retried `mirror_registry`; `target_registry`'s fixture
still has its own skip as a fallback for the case the retry genuinely runs
out — an explicit, visible skip naming the observed cause, which the task
message accepts as a legitimate red outcome). `prod-registry` has no pytest
fixture anywhere in `test/` (confirmed: zero matches for
`prod_registry`/`prod-registry`/`PROD_REGISTRY` outside
`docker-compose.yml`'s own comments), so the parenthetical in the original
finding about it is a no-op — nothing to extend there.

**Proof the fix is real, not a check that always passes.** `_wait_for_reachable`
is exercised by three pure unit tests in the new
`test/tests/test_registry_startup_retry.py` (no Docker, no network — the
retry *shape* is what's under test, not a real registry's boot time, which
the acceptance suite already covers end to end). Per `quality-core.md`'s
"Unchecked Green" rule, a green result is only evidence if a red one was
reachable, so the check below was proven by mutating the real fix in
`test/conftest.py` to a no-retry stub, watching it fail, then restoring it —
not merely by running the new tests once.

Red observation (mutated `_wait_for_reachable` to `return is_reachable()` —
no loop, no retry — then ran the new unit tests):

```
_______ test_wait_for_reachable_retries_until_the_service_comes_up __________
    ...
    sleeps: list[float] = []
    result = conftest._wait_for_reachable(is_reachable, attempts=10, delay_seconds=0.5, sleep=sleeps.append)

>       assert result is True
E       assert False is True

tests/test_registry_startup_retry.py:69: AssertionError
_______ test_wait_for_reachable_gives_up_after_exhausting_every_attempt ________
    ...
    result = conftest._wait_for_reachable(is_reachable, attempts=3, delay_seconds=0.1, sleep=sleeps.append)

    assert result is False
>       assert calls == 3, "every attempt is spent before giving up"
E       AssertionError: every attempt is spent before giving up
E       assert 1 == 3

tests/test_registry_startup_retry.py:85: AssertionError
=========================== short test summary info ============================
FAILED tests/test_registry_startup_retry.py::test_wait_for_reachable_retries_until_the_service_comes_up
FAILED tests/test_registry_startup_retry.py::test_wait_for_reachable_gives_up_after_exhausting_every_attempt
========================= 2 failed, 1 passed in 0.05s ==========================
```

Green observation (mutation reverted, real `_wait_for_reachable` restored —
`git diff --stat` confirmed the restore matched the original edit exactly
before this run):

```
tests/test_registry_startup_retry.py::test_wait_for_reachable_returns_true_immediately_when_already_up PASSED [ 33%]
tests/test_registry_startup_retry.py::test_wait_for_reachable_retries_until_the_service_comes_up PASSED [ 66%]
tests/test_registry_startup_retry.py::test_wait_for_reachable_gives_up_after_exhausting_every_attempt PASSED [100%]

============================== 3 passed in 0.02s ===============================
```

The mutation is real, not a script that reports success unconditionally: it
edited the live line in `test/conftest.py` (`git diff --stat` before/after
shows the same 42-insertion/21-deletion diff both times, confirming the
restore landed byte-identical to the pre-mutation state), and two of the
three tests independently caught it — the third (`returns_true_immediately`)
correctly stayed green because a no-retry stub still satisfies "succeeds on
the first call".

Aside, not load-bearing: before either of the above, the very first attempt
to run the new test file failed for an unrelated reason — a bare `import
conftest` resolved to `test/tests/conftest.py` (a *different* file, the
function-scoped fixtures module, which also happens to be named `conftest`)
rather than `test/conftest.py`, because pytest's "prepend" import mode puts
`test/tests/` on `sys.path` ahead of the `pythonpath` entries. Fixed by
loading `test/conftest.py` by explicit path (the same `importlib`-by-path
convention `_load_fake_forge_module` already uses one directory over), with
the loaded module registered into `sys.modules` under a dedicated name before
execution — required because the module defines `@dataclass class
MockHelper`, and `dataclasses` resolves the owning module by name from
`sys.modules` while the class body runs.

### Finding 2 — `--canonical-tag` default-on, untested (HIGH)

Added a paired positive/negative acceptance test at the OCI wire level (not
just the existing `crates/ocx_cli/src/options/canonical_tag.rs` unit tests,
which only prove clap parses the flag correctly, not that the CLI actually
writes or suppresses the tag at a real registry):

- `test_canonical_tag_default_writes_a_digest_named_tag` — plain copy, no
  flag: asserts `canonical_tags_written == [<tag>]` in the JSON report *and*
  that the target registry actually serves that tag. The expected tag is
  computed from the real leaf digest the copy produced
  (`digest.replace(":", ".")`), not hardcoded, confirmed against
  `push_canonical_tag`'s construction in `crates/ocx_lib/src/oci/client.rs`
  (`format!("{algorithm}.{hex}")`, where the digest is the *leaf platform*
  manifest's digest inside the merged index, not the index's own digest —
  read `push_canonical_tag` and `platform_manifest_digest` to confirm before
  writing the assertion).
- `test_no_canonical_tag_suppresses_it` — `--no-canonical-tag`: asserts
  `canonical_tags_written == []` *and* that the tag is genuinely absent at
  the registry, so the test cannot pass against a build that merely omits
  the tag from the report while still writing it.

### Finding 3 — two-phase write order has no acceptance test (HIGH)

Read `crates/ocx_lib/src/publisher/copy.rs::run` to confirm the exact shape:
`if request.dry_run { continue; }` sits *inside* the per-platform loop,
before phase 1 (`copy_leaf`) ever runs — so a dry run skips both content and
tag/index writes, not just the tag pointer the existing
`test_dry_run_reports_the_plan_and_writes_nothing` test checks (it only
asserts the tag is *absent*, which is silent on whether an *existing* index
at that tag was touched).

Added `test_dry_run_leaves_the_targets_existing_index_byte_for_byte_unchanged`:
pre-populates the target with an unrelated platform under the same tag
(same pattern as `test_copy_merges_into_the_target_index_instead_of_replacing_it`),
snapshots the raw index bytes and digest before a dry-run copy of a
*different* platform, and asserts both are byte-identical afterward — the
cheap, direct observable the review asked for, distinct from (and a
prerequisite complement to) the existing tag-absence test.

### Finding 4 — exit 64 vs 65 for "no platform matches" (HIGH, RED until merged)

**Resolved by team-lead mid-task** (relayed after `review_r1_spec_package_copy.md`
finding A7 / `review_r1_testcov_package_copy.md` finding 4): **64 is correct,
the code is wrong.** A caller naming a platform the source does not carry is
an invocation fault (bad argv), not malformed registry data (the manifest is
fine) — `resolve_source_leaves`'s no-matching-platform branch currently
returns `ClientError::InvalidManifest` (classifies to `ExitCode::DataError`,
65, per `crates/ocx_lib/src/oci/client/error.rs`), when `CopyError::Usage`
already exists for exactly this class. WP-D is reclassifying it; WP-G is
leaving the doc row (`website/src/docs/reference/command-line.md`'s copy
exit-code table) at 64 as-is.

`test_a_platform_the_source_does_not_offer_is_a_usage_error` already asserted
64 (the documented contract) before this resolution arrived, so no test
change was needed — only the docstring, which previously hedged on which
side was wrong and now states the decision plainly, naming WP-D as the
dependency. It stays red until WP-D lands. Uses a fixed, deterministic
candidate-platform list filtered against the one platform the source build
actually offers (never an arbitrary `[0]`/`next()` pick, per
`subsystem-tests.md`'s "Unfalsifiable Greens" table), and asserts the target
is untouched — same "caught before the target is contacted" contract the
other usage-error tests pin.

**Added**, same message from team-lead, covering `review_r1_ux_package_copy.md`
finding A2 — the user-facing half of the same defect (a `--platform` typo is
today reported as a broken source manifest):
`test_a_platform_typo_names_the_platform_not_the_manifest`. Asserts exit code
64 and stderr content *separately* (TEST-10), that stderr names the exact
value the caller typed, and that it does not contain the word "manifest".
Also RED until WP-D lands, same fix as the exit-code test above.

### Finding 5 — convergence gaps #1, #6, #9

- **#1** `test_copy_within_the_same_registry_to_a_different_repository` —
  `-i` targeting a different repo at the *same* registry, multi-platform
  source (two platforms merged onto one source tag), asserting the new
  repo's index carries both. Every other cross-registry test in this file
  exercises `--to`, which by design only ever rewrites the host — this is
  the one shape none of them cover.
- **#6** `test_a_second_copy_after_the_source_gains_a_platform_adds_only_the_new_one`
  — copy once (single platform, `added`), grow the source to a second
  platform, copy again with no `--platform` filter, and assert dispositions
  are exactly `{host: "unchanged", other: "added"}` — the source growing a
  platform between two copies must not force a re-fetch of the platform
  already at the target.
- **#9** `test_offline_refuses_before_touching_the_network` — `--offline`
  before the `package copy` subcommand, asserting exit 81
  (`PolicyBlocked`), distinct from the 64/79 refusals. Confirmed by reading
  `crates/ocx_cli/src/command/package_copy.rs::execute` that the usage-error
  structural checks (digest/tag presence) run entirely locally, before
  `context.remote_client()?` is ever called, and that under `--offline`
  `Context` never constructs a client at all (`remote_client()` returns
  `Err(Error::OfflineMode)` unconditionally) — so the source need not exist
  at any registry for this refusal to fire, and the test does not publish
  one.

### Finding 6 — WARN items

- **Tolerance band** (`test_copy_merges_into_the_target_index_instead_of_replacing_it`,
  was `assert dispositions[host] in {"added", "replaced"}`): narrowed to
  `== "added"` — the fixture only ever pre-populates the target with the
  *other* platform, so `host` is unambiguously fresh there; a band admitting
  `"replaced"` too would have passed against a build that always reported
  `"replaced"`.
- **`_target_has_tag`'s bare `except Exception`**: narrowed to `except
  ValueError`, the one exception `oras.client.OrasClient.get_manifest`
  actually raises on a non-2xx response (`_check_200_response` — confirmed
  by executing the installed `oras` package's source directly). A broader
  catch would also silently report "absent" for a connection failure or a
  bug in the fetch helper itself.
- **Missing positive controls on the two flagged negative assertions**: both
  `test_dry_run_reports_the_plan_and_writes_nothing` and
  `test_a_digest_source_without_a_platform_is_a_usage_error` now assert
  `_target_has_tag` returns `True` for a tag known to exist (at the source
  registry) immediately before trusting its negative result — so neither
  negative assertion can pass vacuously against a helper that always returns
  `False`.
- **Missing stream assertions on the four 64-tests and the one 79-test**: all
  five now assert on `result.stderr` content, not just the exit code, each
  against a message substring read out of the actual source (never guessed):
  `"carries no platform"` and `"carries no tag"` from
  `crates/ocx_cli/src/command/package_copy.rs::execute`'s structural checks,
  `"names an image index by digest"` from
  `crates/ocx_lib/src/publisher/copy.rs::resolve_source_leaves`, `"manifest
  not found"` + the literal tag `"9.9.9"` from `ClientError::ManifestNotFound`'s
  `#[error("manifest not found: {0}")]`. The `--to`/`--identifier` conflict
  test asserts on the two flag spellings (`"--to"`, `"--identifier"`) clap's
  own conflict message must echo, rather than pinning clap's exact sentence
  across a version bump.

### Finding 7 — SUGGEST items

- **Idempotency positive control**: `test_a_repeated_copy_reports_unchanged`
  now asserts the *first* pass's `blobs.uploaded > 0` before asserting the
  second pass's `== 0` — otherwise the second assertion would pass just as
  well against a copy that never uploads anything on either pass.
- **`setups.py`/`docker-compose.yml` name-pinning note**: **no-change-needed**.
  `prod-registry` has no pytest consumer anywhere in `test/` (see Finding 1),
  and `test/recordings/setups.py` is outside my file set (`test/src/registry.py`
  is the only editable helper module, and it needed no change here). The
  `docker-compose.yml` comment on `prod-registry` already explains why it
  exists unconsumed (a third real host for the `--to` shape the docs
  describe, deliberately not wired into a fixture) — nothing to reconcile.

## Red until merged

| Test | Waits on |
|---|---|
| `test_a_platform_the_source_does_not_offer_is_a_usage_error` (`test_package_copy.py`) | WP-D reclassifying `crates/ocx_lib/src/publisher/copy.rs::resolve_source_leaves`'s no-matching-platform refusal from `ClientError::InvalidManifest` (65) to a usage-shaped error (64). Decision confirmed by team-lead: 64 is correct, the doc stays as-is. Confirmed not yet landed on any sibling worktree as of `dfcdcb98`. |
| `test_a_platform_typo_names_the_platform_not_the_manifest` (`test_package_copy.py`) | Same WP-D fix as above. Covers `review_r1_ux_package_copy.md` finding A2 — the same defect from the user's side (stderr names the platform, not the manifest). |
| `test_copy_merges_into_the_target_index_instead_of_replacing_it` (`test_package_copy.py:222`) | WP-B adding `#[serde(rename_all = "kebab-case")]` to `Disposition` (subsystem-cli-api.md "Typed Enums Over Strings") and WP-D holding the JSON report field typed instead of pre-stringified. The one disposition value affected is the multi-word one: `"kept (not in source)"` → `"kept-not-in-source"`; `added`/`replaced`/`unchanged` are already single lowercase words and unaffected. Spelling relayed by team-lead from a WP-B artifact that does not exist in this worktree yet (`.claude/artifacts/fix_wpB_addressing_package_copy.md` absent as of this write) — **unconfirmed**, flagging per team-lead's own instruction so the merge gate is the actual check. `report["status"]` (line 332, asserting `"planned"`) is also becoming a typed `CopyStatus` enum per the same message, but `"planned"`/`"copied"` are already the lowercase single-word forms so that assertion needs no textual change and is not listed as a separate red row. |

Every other test added or changed in this pass is expected to be green
today against a correctly built binary; none of them depend on a fix from
another WP.

## Verification

- **Required gate**: `cd test && uv run pytest tests/test_package_copy.py -v --collect-only` — **24 tests collected, 0 errors** (16 original + 8 new: 2 canonical-tag, 1 dry-run-byte-identity, 1 same-registry-different-repo, 1 second-copy-new-platform, 1 missing-platform usage error, 1 platform-typo message, 1 offline). Re-verified after the two course-correction messages from team-lead.
- Also ran `cd test && uv run pytest tests/ -q --collect-only` (whole suite, `OCX_TESTS_NO_REGISTRY=1`) — **2294 tests collected, 0 errors** — confirms the edits do not break collection of any sibling test module.
- `python3 -m py_compile` on all three touched/added files — clean.
- The new pure unit test (`test_registry_startup_retry.py`, 3 tests) was actually **executed** (not just collected) — green after the fix, and independently proven able to fail via the mutation described under Finding 1.
- **Not attempted**: a real registry-backed run of `test_package_copy.py`.
  `test/bin/ocx` does not exist in this worktree (no build has been run
  here), and building it (`--features ocx/__testing`, per
  `subsystem-tests.md`) is outside my declared file set and this task's time
  budget. Docker is available (`docker info` succeeds), so a real run is
  possible once a current binary exists — left to whichever WP merges last
  and runs `task test`/`task test:parallel`.
- `ruff` was not available in this environment (`uv run ruff` — "No such
  file or directory"); not attempted, not claimed.

## Commits

- `test(harness): retry target_registry on the cold-start race, matching mirror_registry`
- `test(copy): close acceptance gaps on canonical tags, dry-run atomicity, three convergence cases, and five weak assertions`
