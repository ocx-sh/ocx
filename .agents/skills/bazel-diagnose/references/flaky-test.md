# Flaky tests and silent green

Read this while running section E. It holds the detection command and why the
obvious alternatives manufacture a green, the sizing and sharding contract, the
environment invariants a test may rely on, and the four distinct bugs that all
present as an empty coverage report.

Contents: [Detect, never mask](#detect-never-mask) ·
[Sizing and timeouts](#sizing-and-timeouts) ·
[Sharding](#sharding) ·
[Environment invariants](#environment-invariants) ·
[Tags that change what ran](#tags-that-change-what-ran) ·
[The Bazel 8 to 9 boundary](#the-bazel-8-to-9-boundary) ·
[Empty coverage is four bugs](#empty-coverage-is-four-bugs)

The test contract is normative and stable: the 8.7.0 and 9.1.0 test-encyclopedia
snapshots differ in one substantive place, named below. Any other version-specific
claim about size, timeout, sharding, tags or the environment invariants is
fabricated. Primary source:
<https://bazel.build/reference/test-encyclopedia>.

## Detect, never mask

```
bazel test --runs_per_test=100 --runs_per_test_detects_flakes //the:test
```

That is the detection command, and the only one (BZL-TEST-06). Both retry
mechanisms — the `flaky = True` attribute and `--flaky_test_attempts` — stop at
the **first success** and skip the remaining attempts, so a 50%-flaky test
"passes" `--flaky_test_attempts=10` essentially always. Recommending either for
detection manufactures a false green, which is a correctness defect rather than a
style preference.

How the output reads:

- Mixed PASSED/FAILED across the runs, with a `FLAKY` summary — flake confirmed
  on **this host**. Record the host, the strategy and the ratio.
- All 100 pass — evidence about this host only. If the report came from CI, the
  next run belongs on the lane that reported it, under that lane's own strategy
  and resource pressure. An all-green local run is not a refutation.

An existing `flaky = True` in the tree is not automatically a defect, but it must
carry a comment naming the root cause and a tracking link; a bare one is the
finding (BZL-TEST-05).

## Sizing and timeouts

`size` sets three things: the RAM estimate (20/100/300/800 MB by size), **always
exactly 1 CPU**, and the default `timeout`. An unset `size` never breaks
correctness — it moves local scheduling and the default timeout only, so a
defaulted `medium` on a millisecond test is accepted rather than filed
(BZL-TEST-01).

Two consequences for a flake:

- A test that got slower and now trips its size-derived default timeout looks
  intermittent on a loaded machine and reliable on an idle one. Re-check declared
  `size`/`timeout` against measured runtime whenever the test's cost changes
  materially (BZL-TEST-02).
- A test needing more than one core gets the `cpu:n` tag, **never a larger
  `size`** (BZL-TEST-03). Bumping `size` to buy parallelism buys a timeout and a
  RAM estimate instead, and hides the real requirement.

Re-check the same numbers under `bazel coverage`: with
`--experimental_split_coverage_postprocessing` at its real default of `false`
(verified on 8.7.0, 8.8.0 and 9.2.0), the collector runs in the **test's own
spawn**, sharing that test's timeout. This is Bazel core behaviour for every
language, not one ruleset's quirk. The *combined* report is a separate action
outside any single test's budget — conflating the two budgets the wrong thing.

## Sharding

`shard_count` is safe only if the test runner actually touches
`TEST_SHARD_STATUS_FILE`. A runner that ignores it does **not** silently run every
test in every shard, whatever the Build Encyclopedia's attribute prose says — that
prose is stale in both the 8.7.0 and 9.1.0 snapshots. The shipped behaviour is a
hard `LOCAL_TEST_PREREQ_UNMET` that force-fails a shard which would otherwise
have passed (BZL-TEST-04).

So a "flake that started when we added sharding" is usually not a flake: confirm
the runner touches the file, and describe the failure as the hard prerequisite it
is. Cite the test-encyclopedia, never the attribute table.

## Environment invariants

A test that writes outside its sandbox is the most common source of
order-dependent and parallelism-dependent failures:

| Invariant | Check | Rule |
|---|---|---|
| Output, temp files and state go only under `$TEST_TMPDIR` or `$TEST_UNDECLARED_OUTPUTS_DIR` | `grep -rnE '/tmp/|\$HOME|/var/tmp' <test sources>` — any absolute path outside those two variables is the finding; EMPTY clears only the files searched | BZL-TEST-12 |
| The runfiles tree is never mutated during a run, and no assertion depends on filesystem atimes | Read each test for in-place writes to a runfiles path | BZL-TEST-13 |
| A test reaching the network is tagged `requires-network`; one whose only network use was a repository-rule fetch is **not** | `grep -rn 'requires-network' --include='BUILD*' .` against what the test actually opens | BZL-TEST-09 |
| A "no network" claim is proved from a **cold repository cache**, not from a green run | Re-run after `bazel clean --expunge` with the cache emptied | BZL-TEST-21 |

`HOME` is unset in the action environment on both majors, so a test that assumes
it exists fails differently under `bazel test` than under a direct invocation —
that difference is the diagnosis, not a flake.

## Tags that change what ran

Measured on both majors against a warm `--disk_cache` and cross-read against the
execution log (BZL-TEST-08):

| Tag | Runner | Rebuild behaviour |
|---|---|---|
| `no-sandbox` | `local` | Still cacheable — **hits** on rebuild |
| `local` | `local` | Marked uncacheable — **re-executes every build** |
| `no-remote-cache` / `no-remote` | `linux-sandbox` | Disk cache untouched — hits |
| `no-cache` | `linux-sandbox` | Misses; still remotable |
| `external` | — | **Test-only.** With `--cache_test_results=yes` the tagged test re-runs every invocation while its untagged sibling reports `(cached)`. Zero effect on a build action |
| `no-remote-cache-upload` | — | Suppresses only the `PUT /ac/` result mapping; the CAS content upload still fires |

Never treat `no-remote-cache`/`no-remote-cache-upload`, `external`/`requires-network`
or `no-sandbox`/`local` as interchangeable pairs. The last two rows are
**invisible to a `--disk_cache`-only probe** — such a probe reads blind, never
"confirmed".

A flake that appears the moment a `no-sandbox` or `local` tag is added is the tag
doing its job: the test was relying on isolation it no longer has. Removing the
tag to make the red go away is editing the check.

For a Starlark action, the same strings do **not** arrive through the target's
`tags` reliably — set `execution_requirements` on the `ctx.actions.*` call
instead, and remember one `tags` list cannot express different needs for
different actions of the same rule (BZL-TEST-07).

## The Bazel 8 to 9 boundary

Exactly one substantive change: 9.0.0 adds a mandatory **implicit toolchain
requirement** on `@bazel_tools//tools/test:default_test_toolchain_type`. The
`test` exec group itself, and `test.<key>`-scoped `exec_properties`, already
existed at 8.8.0 — describing the exec group as new is wrong (BZL-TEST-28).

Satisfy it by registering the test's target platform as an execution platform, or
by shipping a custom toolchain for that type. The escape hatch
(`--@bazel_tools//tools/test:incompatible_use_default_test_toolchain=false`) is
marked for removal with **no version and no tracking issue** — a documented gap,
watchable only through release notes.

`--combined_report` flipped `none` → `lcov` at 9.0.0. A matrix spanning both
majors produces a merged report on the 9.x leg and none on the 8.x leg unless the
flag is passed explicitly (BZL-TEST-23). Every other coverage flag is unchanged.

## Empty coverage is four bugs

`bazel coverage` exits **0** on an empty report and the test still reports PASS;
the encyclopedia does not mention coverage at all. Never gate on the exit code.
The separator is the `DA:` record count inside the produced
`_coverage_report.dat` (BZL-TEST-25):

```
grep -c '^DA:' bazel-out/_coverage/_coverage_report.dat
```

`0` with a green run is the finding. Then differentiate:

| Cause | Confirming signal | Rule |
|---|---|---|
| The instrumentation filter matched nothing | Bazel prints `INFO: Using default value for --instrumentation_filter: "…"` — with the flag unset it is **computed from the packages of the test targets named on the command line**, not from the code under test, so a `//lib/foo` + `//tests/foo` layout instruments nothing | BZL-TEST-22 |
| No runfiles tree on a Windows leg for a ruleset that needs one | Absent `--enable_runfiles` on that leg — but rules_python forces it since 1.9.0 via a transition, so its absence there is **not** a finding | BZL-TEST-24 |
| The resolved Python interpreter has no bundled `coverage` wheel | rules_python 2.3.3 bundles CPython 3.9–3.14 only; outside that range the only trace is an analysis-time warning | BZL-TEST-26 |
| A clang/LLVM toolchain emitted raw `.profdata` the LCOV merger cannot read | Add `--experimental_generate_llvm_lcov` on that leg | BZL-TEST-27 |

"The report is empty, so fix the filter" is wrong three times out of four.

**Documented gap:** `py_test` and `rust_test` coverage on Windows is unverified
upstream, not merely unchecked — rules_js is the only ruleset documenting the
failure with exact log text, and rules_rust 0.74.0 disclaims Windows support in
its own index. Settling it needs one `bazel coverage` run of a trivial test of
each kind on a real Windows executor, with and without `--enable_runfiles`.
